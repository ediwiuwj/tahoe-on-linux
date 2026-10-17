# Pendientes

Lista de trabajo del macOS 26 completo sobre tahoe-run, en orden de prioridad. Se actualiza al cerrar o abrir tareas.

**En pausa desde el 14 de octubre de 2026.** Cómo retomarlo: README, «Cómo retomarlo».

## Por dónde seguir
1. **Iniciar sesión y llegar al escritorio**: ya se puede escribir la contraseña en la pantalla de inicio de sesión
   desde tahoe-view (teclado y ratón hechos). Siguiente: ver qué pasa tras iniciar sesión (Dock, Finder, escritorio).
2. **`diskutil list` en la Recovery** se cuelga a veces (mucho menos tras arreglar los hilos de cola): mirar con
   `tools/diag/discos.sh`.
3. Lo demás, en la lista de abajo.


## Hecho recientemente
- **Teclado y ratón.** En macOS el sistema de eventos HID vive en WindowServer, que busca en IOKit los servicios con
  `HIDServiceSupport`. tahoe-run publica un teclado y un puntero (IOHIDEventDriver) con su IOHIDEventServiceUserClient:
  la cola de eventos es un IODataQueue compartido y cada evento un IOHIDSystemQueueElement (formatos de IOHIDFamily,
  y la disposición de teclado, puntero y rueda medida en IOKit.framework). tahoe-view manda las teclas (por posición
  física: la distribución la pone macOS), la posición absoluta del ratón, los botones y la rueda por
  `/dev/shm/tahoe-input-<id>`; `tools/diag/entrada.py` hace lo mismo desde la terminal para probar.
- **Pantalla de inicio de sesión visible.** Tres arreglos: (1) los hilos de cola encolaban dos trabajos para un solo
  hilo libre, y si el primero se bloqueaba llamando al propio proceso el segundo no salía nunca (configd se quedaba sin
  responder y con él diskarbitrationd, StorageKit, diskutil y SecurityAgent); (2) la respuesta del núcleo emulado en
  `mach_msg2` iba en 1 KB de pila e `io_connect_method` devuelve hasta 4 KB: SecurityAgent desbordaba la pila y moría
  (loginwindow salía detrás: «AuthorizationRef doesn't have a username»); (3) métodos de IOSurface que CoreVideo repetía
  sin fin al fallar (SetValue 9, SetPurgeable 0x14, SetBulkAttachments 0x1b, SetCoreVideoBridgedKeys 0x39, 0x2b).
  Diagnóstico nuevo: un SIGABRT del propio tahoe-run deja la pila del emulador en el registro (`addr2line`).
- LaunchServices (lsd) ya guarda su base de datos: antes no podía («could not write out system content database») y en
  cada arranque volvía a registrar todas las apps (de 5 a 30 minutos de espera, y la pantalla de inicio llegaba cuando
  quería). Causas: `fgetattrlist` estaba con el número de `fsetattrlist`, `setattrlist` rechazaba la clase de protección
  y las ACL, y faltaban `access_extended` y `quotactl`. Quedan en la copia de macOS unos 200 temporales viejos
  (`…/com.apple.LaunchServices.dv/*(xxxx~).csstore`, 1,3 GB) de cuando fallaba: se pueden borrar.
- La pantalla no está «offline»: los mensajes «Setting offline display» de CoreDisplay son del orden de arranque; con
  puntos de ruptura (BRKP) se ve que IOFBUpdateConnectState la da por conectada y MPInitScreen la pone online.
- Aviso MACH_NOTIFY_NO_SENDERS al soltar el último derecho de envío (antes solo al morir el proceso): se cuentan los urefs
  de cada proceso, también los del receptor. WindowServer libera así las vallas de CoreAnimation (de ~85 plazos agotados
  a 5 en 25 minutos). Trazas nuevas en msh.sh: NS=1 y SR='<procesos>'.
- WindowServer ya no se cae y dibuja: devolvía 0 como puerto en `MACH_RCV_LARGE_IDENTITY` (CoreAnimation reintentaba en
  el puerto 0 y abortaba), faltaba `mach_port_extract_right` (loginwindow, SecurityAgent y cualquier app abortaban al
  mostrar su primera ventana: «Assertion failed: (scheduled)»), y la tabla de puertos de todo el sistema (8192) se llenaba
  a los pocos minutos de arrancar la parte gráfica (ahora 32768, recorriendo solo lo usado).
- Camino hasta la ventana de inicio de sesión: loginwindow ya no intenta lanzar el Asistente de configuración
  (`/.resolve/<opciones>/ruta`, la sintaxis de XNU con la que el Asistente mira `.AppleSetupDone`, no se entendía),
  `proc_listpids` implementado, `ptrace(PT_DENY_ATTACH)` aceptado (SecurityAgent salía con «Unable to deny attach»),
  las propiedades que se escriben en IOKit se guardan (IOFBConfig: CoreDisplay ya elige el modo 1280×800), el
  framebuffer se llama IONDRVFramebuffer (con «Intel» en el nombre CoreDisplay usaba IOPresentment desde el arranque),
  entradas de memoria nuevas (`MAP_MEM_NAMED_CREATE`, la memoria compartida de HID) y `TAHOE_BOOTARGS` para los
  argumentos de arranque.
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
5. **Metal 2 sobre Vulkan (Rust)**. La GPU del PC (Intel HD 4400, Haswell, Vulkan 1.2 de Mesa `hasvk`) **alcanza para
   Metal 2 como lo tenían los Mac con Intel**: `tahoe-gpu informe` da 18 requisitos nativos, 8 emulables en el
   traductor de shaders (half, enteros de 64 y 16 bits, punteros, SIMD-group, texturas read_write...) y 4 que faltan,
   de hardware o poco usados (texturas de 16384, argument buffers tier 2 —los Mac con Intel solo tienen tier 1—,
   escritura en buffers desde vertex shaders y raster order groups). Un driver Vulkan propio no daría lo que no tiene
   el hardware, así que **no se toca el driver del sistema**: lo que falte se emula en Rust en nuestro lado.
   Los cinco pasos, todo en Rust:
   1. **Traductor AIR → SPIR-V** (`tahoe-gpu/src/air`): hecho. Lector de bitcode, intérprete de referencia, SCCP,
      control de flujo estructurado («Beyond Relooper»; la máquina de estados queda para CFG irreducibles), texturas de
      todas las formas, atómicas, SIMD-group y quad (con shuffles: la GPU no tiene aritmética de subgrupos), bfloat16.
      QuartzCore: 156/156 válidos con spirv-val (las 24 restantes son de GPU de Apple: imageblocks, render_target).
      Banco GPU contra CPU (`tahoe-gpu probar`): en una muestra de 339 kernels de macOS, 169 dan lo mismo en la GPU y
      en el intérprete. Falta: punteros en memoria (argument buffers), visible_function_table, imageblocks.
   2. **macOS ve una GPU Metal**: hecho. Con `TAHOE_METAL`, tahoe-run publica un IOAccelerator con
      `MetalPluginName` y lleva `/System/Library/Extensions/TahoeMetal.bundle` a nuestro bundle;
      `MTLCreateSystemDefaultDevice()` devuelve `TahoeMTLDevice` «Tahoe GPU (Intel(R) HD Graphics 4400)». Canal:
      socket Unix con el servidor de GPU (`tahoe-gpu servir`, el único proceso con Vulkan) y memoria compartida en
      `/dev/shm` (buffers e IOSurfaces).
   3. **Driver Metal** (`tahoe-metal`, ver su LEEME): cómputo y render de punta a punta dentro de macOS (`metalprobe
      computo` y `metalprobe render`: un kernel de un metallib del sistema llena un buffer, y un triángulo con shaders
      de QuartzCore sale CORRECTO en la HD 4400). Hecho: buffers, bibliotecas, function constants (interceptando
      MTLFunctionConstantValues), pipelines de cómputo y de render con vertex descriptor y mezcla, texturas (normales y
      sobre IOSurface), samplers, encoders de cómputo, blit y render. `registryID` es el ID en IOKit del IOAccelerator
      (CoreDisplay busca el dispositivo con `MTLCopyDeviceForRegistryID`). Traza de depuración: con
      `/private/var/tmp/tahoe-metal.traza`, libobjc apunta en `/tmp/msgSends-<pid>` cada par clase/selector que usa el
      proceso (90 s). Los shaders de WindowServer y CoreDisplay ya se traducen: SkyLight 28/31 (faltan los 3 «InPlace»,
      que leen el color de destino, `air.render_target`) y CoreDisplay 4/4.
   4. **WindowServer/CoreAnimation por la GPU**: WindowServer carga el driver, crea el dispositivo y pregunta sus
      capacidades, pero no lo usa. Lo que decide (SkyLight/CoreDisplay de macOS 26):
      - `WSSystemCanCompositeWithMetal`: hay Metal si algún dispositivo de `MTLCopyAllDevices` responde NO a
        `isHeadless` (el nuestro ya lo hace) y entonces se activa el compositor Metal;
      - pero cada pantalla usa `WSGetCompositorForAccelerator(acelerador de la pantalla)`, y con acelerador 0 es el
        compositor por software. Nuestra pantalla es un framebuffer virtual (VFB) hecho a partir del IONDRVFramebuffer:
        hereda el acelerador de su pantalla madre, que no tiene (puerto 0), y `IOAccelCreateSurface` falla;
      - en macOS 26 solo quedan dos caminos de pantalla: VFB e **IOPresentment** (`CoreDisplay::UseIOPresentment`,
        el de los Mac Intel). CoreDisplay ya encuentra el TahoeAccelerator al recorrer los IOAccelerator y busca
        debajo un `IOPresentmentClient`; el MetalDevice de cada pantalla sale de su acelerador
        (`CoreDisplay::CreateMetalDevice` → `IORegistryEntryGetRegistryEntryID` → `MTLCopyDeviceForRegistryID`).
      **Siguiente**: emular IOPresentment en tahoe-run (un `IOPresentmentClient` bajo el TahoeAccelerator, el AGDC
      en la lista del GPUWrangler y las transacciones de display pipe: `DisplayPipeTransactionSetPlaneIOSurface` da la
      IOSurface que hay que mostrar, que el servidor de GPU ya comparte en `/dev/shm`, y tahoe-view la enseñaría).
      Es el camino que usa macOS en los Intel y deja componer a WindowServer con el compositor Metal.
   5. **Metal 2 completo**: lo que quede (argument buffers, samplers constantes de 64 bits, offsets de buffer sin
      alinear, cull mode dinámico, profundidad/estarcido, MSAA...), y pruebas con apps.
6. **Sesión gráfica**: con ella, `screencapture`, `osascript` y `pbcopy` (hoy se caen por no haber sesión).
7. **Servicios que fallan al arrancar**: mobileassetd, softwareupdate; apsd se cae en bucle («ProductType» nil);
   relanzados en bucle: cryptexd, watchdogd, diskmanagementstartup, triald_system, cupsd. Muchas preferencias de
   `/Library/Preferences` y de root se guardan vacías (0 bytes): loginwindow, dock, CoreGraphics... (¿cfprefsd?
   `fcntl F_PREALLOCATE` no está implementado).
8. **Sonido** dentro de macOS (teclado y ratón ya funcionan, ver «Hecho recientemente»). coreaudiod ya arranca y `say`
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
