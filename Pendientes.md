# Pendientes

Lista de trabajo del macOS 26 completo sobre tahoe-run, en orden de prioridad. Se actualiza al cerrar o abrir tareas.

## Hecho recientemente
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
   - Servicios en bucle: KernelEventAgent arreglado. digest-service sigue (no abre su base de Kerberos por
     OpenDirectory); xprotectd y endpointsecurityd necesitan EndpointSecurity (`es_new_client` da error interno);
     watchdogd, por investigar. Otros que se relanzan: locationd, cryptexd, diskmanagementstartup, apsd, cloudd.
   - Discos, lo que falta: capacidades en `diskutil apfs list` («ERROR»: APFSExtendedSpaceInfo, selector 8 del cliente de
     AppleAPFSContainer, devuelve un diccionario cuyas claves no se conocen; con una respuesta incompleta storagekitd se
     cae, así que de momento el contenedor no admite conexión) y `bless --info` (Preboot no tiene la carpeta de arranque
     `<UUID>/System/Library/CoreServices` que crea el instalador; con la propiedad IOAPFSPreBootDevice también falla
     `--getBoot`).
   - Esperas parecidas: `log show` ya no se cuelga pero tarda 65–165 s; `system_profiler` y `lwp-request` se cuelgan.
   - General: apagar los registros de diagnóstico fuera de las pruebas; índices en las tablas en vez de recorrerlas;
     mensajes Mach en Rust; arranque de procesos (servicios bajo demanda) más ligero.
2. **Caídas**:
   - `shortcuts`: «Assertion failed: (platformBinary)»; `csops` no dice que es un binario de plataforma de Apple.
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
7. **Servicios que fallan al arrancar**: locationd, mobileassetd, softwareupdate.
8. **Teclado y ratón** dentro de macOS.
9. **Detalles**:
    - `top`: columnas de memoria y puertos por proceso (`mach_vm_region` 4816, `mach_port_space_basic_info` 3235).
    - `iostat`: la segunda muestra de CPU sale a 0.
    - `necp_open` (red).
    - `security list-keychains`.
    - `zprint`: pide las zonas del núcleo de XNU, que no existen en Linux (probablemente se queda así).
10. **Baja prioridad**: Java (`java`, `javac`... se caen en vez de decir que no hay Java instalado), `kmutil` y lo
    relacionado con kexts.
11. **README** al día y **PR #8**: fusionar cuando la shell del completo supere de forma estable lo de la Recovery
    (56 de 79 órdenes; ahora sale entre 56 y 61).
