# Pendientes

Lista de trabajo del macOS 26 completo sobre tahoe-run, en orden de prioridad. Se actualiza al cerrar o abrir tareas.

## Hecho recientemente
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
   - `diskutil`: funciona, pero la primera vez tarda unos 100 s (79 s la segunda, 8 s la tercera). Algo espera hasta
     agotar un plazo: trazar el primer `diskutil list` y sus servicios (diskmanagementd, storagekitd, diskarbitrationd) y
     quitar esa espera. Después, mostrar los volúmenes del macOS completo (Sistema, Datos, Preboot) en vez de
     «macOS Base System»; `bless --info` también pide Preboot.
   - Esperas parecidas: `log show`, `system_profiler` y `lwp-request` se cuelgan.
   - General: apagar los registros de diagnóstico fuera de las pruebas; índices en las tablas en vez de recorrerlas;
     mensajes Mach en Rust; arranque de procesos (servicios bajo demanda) más ligero.
2. **`id <usuario>`**: «failed to retrieve group list: Undefined error: 0» (la consulta de grupos del usuario a
   opendirectoryd falla en el emulador).
3. **Caídas**:
   - `shortcuts`: «Assertion failed: (platformBinary)»; `csops` no dice que es un binario de plataforma de Apple.
   - `screen` (sin terminal debería salir con «Must be connected to a terminal»).
   - `login` (debería pedir el nombre o salir con error, no caerse).
   - El resto de los SIGILL: `kextutil`, `vpnd`, `snmptrapd`, `kgetcred`, `json_xs5.34`, `kill.d`,
     `net-snmp-create-v3-user`, `package-stash-conflicts`.
4. **Repetir el inventario completo** (`tools/inventario_total.sh`, 1167 programas) con todo lo arreglado
   (antes: 615 responden, 190 fallos del emulador).
5. **Pantalla**: que WindowServer dibuje y se vea el escritorio (o el Asistente, si se deshace la preconfiguración);
   fallo intermitente del nib de Language Chooser.
6. **Metal 2 sobre Vulkan (Rust)**: driver Metal propio publicado en IOKit (IOAccelerator, `MetalPluginName`), órdenes de
   Metal a Vulkan con `ash`, shaders AIR→SPIR-V (`metal2vulkan`); primero la composición de WindowServer y CoreAnimation
   (Liquid Glass), después Metal 2 completo y lo que pida macOS de Metal 3 (ampliando `hasvk` de Mesa si hace falta).
7. **Sesión gráfica**: con ella, `screencapture`, `osascript` y `pbcopy` (hoy se caen por no haber sesión).
8. **Servicios que fallan al arrancar**: locationd, mobileassetd, softwareupdate.
9. **Teclado y ratón** dentro de macOS.
10. **Detalles**:
    - `top`: columnas de memoria y puertos por proceso (`mach_vm_region` 4816, `mach_port_space_basic_info` 3235).
    - `iostat`: la segunda muestra de CPU sale a 0.
    - `necp_open` (red).
    - `security list-keychains`.
    - `zprint`: pide las zonas del núcleo de XNU, que no existen en Linux (probablemente se queda así).
11. **Baja prioridad**: Java (`java`, `javac`... se caen en vez de decir que no hay Java instalado), `kmutil` y lo
    relacionado con kexts.
12. **README** al día y **PR #8**: fusionar cuando la shell del completo supere de forma estable lo de la Recovery
    (56 de 79 órdenes; ahora sale entre 56 y 61).
