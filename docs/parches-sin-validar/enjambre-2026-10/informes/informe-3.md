# Informe de la tarea 3 — `login` y `screen` se caen (Pendientes, punto 2 «Caídas»)

**Todo lo de aquí es análisis estático.** No he ejecutado `tahoe-run`, ni la shell de macOS, ni la batería (reglas del
encargo): solo he leído código, desensamblado la `dyld_shared_cache` de macOS 26 (solo lectura) y compilado el
emulador con `nice -n 19 ionice -c3`, `cargo build -j 1` y un único `clang`.

Referencias de XNU: `~/macos26/src/xnu` (solo lectura). Para la libc de macOS (que no está en el árbol de
XNU) he desensamblado la caché compartida con las herramientas del repo (`tools/dsc_dis.py`, `tools/diag/stub.py`) y
las listas de símbolos de `~/.cache/tahoe-syms`.

> **Nota sobre el estado del directorio**: el intento anterior de esta tarea (interrumpido por un fallo de conexión)
> había dejado ya tres cambios en el árbol (`emu_net.c`, `emu_proc.c`, `tahoe_run.c`) y este informe. Los he
> **conservado** y he continuado: he revisado cada cambio contra XNU, he corregido cuatro desviaciones que quedaban
> (§2.2, §2.3, §2.6 y §2.7) y he añadido el diagnóstico que faltaba (§2.8). El diff completo está en
> `../parche-3.diff` (252 líneas, 3 ficheros).

---

## 1. Resumen

Hay **dos cosas distintas** detrás de «`login` y `screen` se caen»:

1. **Desviaciones reales de la semántica de XNU** en la zona `tty`/`ioctl`/`login` que usa `screen` y `login`
   (`getlogin`, `setlogin`, las ioctl de pseudoterminal, `TIOCFLUSH`, `TIOCSCTTY`, la velocidad de `struct termios`).
   Estas las he corregido (§2).
2. **La caída en sí (SIGILL)** no está en las syscalls: el emulador **no deja rastro** de ella (§4). Lo que sí se puede
   hacer sin ejecutar es dejar el mecanismo preparado para que la próxima vuelta del inventario diga la dirección de la
   instrucción ilegal (§2.8) y **acotar qué se puede deducir** (§4).

Correcciones de la tabla:

| # | Qué | Dónde | Referencia de XNU |
|---|---|---|---|
| 1 | `getlogin` (syscall 49) devolvía `ERANGE` y copiaba 6 bytes fijos | `tahoe_run.c` | `bsd/kern/kern_prot.c:1566` |
| 2 | `setlogin` (syscall 50) estaba registrado a **la función equivocada** (`bsd_proc_rlimit_control`) y no guardaba nada | `emu_proc.c`, `tahoe_run.c` | `bsd/kern/kern_prot.c:1619` |
| 3 | Faltaban las ioctl de pseudoterminal `TIOCPTYGRANT`, `TIOCPTYUNLK`, `TIOCPTYGNAME` | `emu_net.c` | `bsd/kern/tty_dev.c:1091-1130` |
| 4 | `TIOCSCTTY` no seguía la regla de XNU (0 si el terminal ya es el de control de la sesión; `EPERM` si la sesión ya tiene terminal) | `emu_net.c` | `bsd/kern/tty.c:1454` |
| 5 | Faltaba `TIOCFLUSH`, y su argumento no era el de XNU | `emu_net.c` | `bsd/kern/tty.c:1155` |
| 6 | La velocidad de `struct termios` estaba fija en 38400 en los dos sentidos | `emu_net.c` | `bsd/sys/termios.h` |
| 7 | `setlogin` miraba la cadena antes que los privilegios (XNU mira primero `proc_suser`) | `tahoe_run.c` | `bsd/kern/kern_prot.c:1619` |
| 8 | `TIOCPTYGRANT`/`TIOCPTYUNLK` devolvían el errno de Linux en vez de `ENOTTY`, y **no desbloqueaban** el esclavo del pty (en Linux está bloqueado: al abrirlo da `EIO`) | `emu_net.c` | `bsd/kern/tty_dev.c:1091-1133` + Linux `TIOCSPTLCK` |

Además:

* se quitó un **doble registro de la syscall 54 (`ioctl`)** (el de `emu_proc.c` era un stub muerto que devolvía
  `ENOTTY` siempre y solo ganaba por el orden de los `init`);
* se instaló un **manejador de SIGILL temprano** para que las caídas del propio emulador durante su arranque dejen
  rastro (§2.8, §4.3).

---

## 2. Lo que cambié, en detalle

### 2.1 `getlogin` (syscall 49) — `tahoe_run.c`

Antes:

```c
static long bsd_getlogin(struct ctx* c) {
	static const char u[] = "root";
	if (c->a[1] < sizeof u) return -34;                   // ERANGE
	return safe_write(c->a[0], u, sizeof u) == (ssize_t)sizeof u ? 0 : -14;
}
```

XNU (`bsd/kern/kern_prot.c:1566`):

```c
	if (uap->namelen > MAXLOGNAME) uap->namelen = MAXLOGNAME;   /* 255 */
	... bcopy(sessp->s_login, buffer, uap->namelen);
	return copyout((caddr_t)buffer, uap->namebuf, uap->namelen);
```

Diferencias: XNU **nunca** devuelve `ERANGE`; recorta el tamaño a `MAXLOGNAME` (255, `bsd/sys/param.h:97`) y copia
**exactamente** ese número de bytes del nombre de la sesión (el resto, ceros). Ahora igual.

Comprobado en la libc de macOS 26 (desensamblado de `libsystem_c.dylib`): `getlogin()` reserva 255 bytes
(`mov $0xff,%esi`) y llama a la syscall 49 con ese tamaño; si la syscall devuelve negativo, lee `errno` y lo publica.

### 2.2 `setlogin` (syscall 50) — `emu_proc.c` + `tahoe_run.c`

`emu_proc.c` registraba la syscall 50 a `bsd_proc_rlimit_control` («sin efecto», devuelve 0). Eso no es la semántica
de XNU: `setlogin` **sí** tiene efecto (es lo que deja el nombre en la sesión del que `getlogin` lo saca). XNU
(`bsd/kern/kern_prot.c:1619`):

1. `proc_suser()` → `EPERM` si no es el superusuario;
2. `copyinstr` con un máximo de `MAXLOGNAME-1` → `ENAMETOOLONG` traducido a `EINVAL`;
3. guarda el nombre en `sessp->s_login` (la sesión).

Ahora hay un `bsd_setlogin()` propio, registrado en la syscall 50, que hace esas tres cosas **en ese orden** (el
`EPERM` se mira antes de la cadena, como en XNU). El nombre de la sesión se guarda **por sesión de Linux** (`getsid`),
que es lo más parecido a la sesión de XNU. Al principio el nombre es `root`, como hasta ahora (todo el arranque corre
como root); a partir de un `setlogin`, `getlogin` devuelve ese nombre.

*Limitación conocida y anotada*: la tabla es por proceso de tahoe-run, así que un `setlogin` hecho en un proceso no lo
ve otro proceso de macOS aunque compartan sesión (en XNU sí se verían), y un `exec` lo pierde (en XNU lo conserva, que
es de la sesión). No lo he resuelto porque exige compartir estado entre procesos como se hace con la región de puertos.

### 2.3 Las ioctl de pseudoterminal que faltaban — `emu_net.c`

En macOS, `grantpt`/`unlockpt`/`ptsname` de la libc **son ioctl**. Verificado desensamblando `libsystem_c.dylib` de la
caché de macOS 26:

```
_grantpt   : ioctl(fd, 0x20007454, 0)      -> TIOCPTYGRANT
_unlockpt  : ioctl(fd, 0x20007452, 0)      -> TIOCPTYUNLK
_ptsname_r : ioctl(fd, 0x40807453, buf)    -> TIOCPTYGNAME (128 bytes)
```

Son exactamente `_IO('t',84)`, `_IO('t',82)` e `_IOC(IOC_OUT,'t',83,128)` de `bsd/sys/ttycom.h:180-182`. El emulador no
los tenía: los tres devolvían `ENOTTY` y **`screen` no podía crear una pseudoterminal**.

Ahora:

* `TIOCPTYGRANT`/`TIOCPTYUNLK`: se comprueba que el descriptor es un maestro de pseudoterminal (`TIOCGPTN`); si no,
  `ENOTTY` como en XNU (no el errno de Linux). Y **desbloquean el esclavo**: en Linux el esclavo de un pty está
  bloqueado hasta que se desbloquea, y al abrirlo sin desbloquear da `EIO` (comprobado con un programa de prueba
  propio, `/tmp/opencode/pty3.c`: «sin desbloquear: esclavo -> -1 (Input/output error)», y tras `TIOCSPTLCK(0)`
  «desbloqueado: esclavo -> 4 (ok)»). Es lo que hacen `grantpt`/`unlockpt` de glibc. Sin esto, `screen` seguiría sin
  poder abrir el esclavo aunque las ioctl existieran.
* `TIOCPTYGNAME`: se devuelve el nombre **de macOS** `/dev/ttysNNN` (Linux lo llama `/dev/pts/N`), que es lo que
  espera el invitado: el emulador ya traducía `/dev/ttysNNN` → `/dev/pts/N` al abrir (`tahoe_dev_alias`) y las
  añadía al listar `/dev`. El número sale de `TIOCGPTN` (el mismo índice del pty) ✓.

### 2.4 `TIOCSCTTY` — `emu_net.c`

XNU (`bsd/kern/tty.c:1454`): si el terminal **ya es** el terminal de control de la sesión, devuelve 0 sin hacer nada;
si el proceso no es líder de sesión, `EPERM`; si la sesión ya tiene otro terminal, `EPERM`.

El emulador lo delegaba tal cual en Linux, cuya regla es distinta (`EBUSY` si la sesión ya tiene terminal), así que un
programa que hacía lo que hace XNU (volver a pedir el terminal que ya tenía) recibía un error. Ahora, antes de delegar,
se consulta `tcgetsid()`: si el terminal pertenece ya a la sesión del proceso, se devuelve 0 (como XNU); si no, se
delega en Linux, traduciendo su `EBUSY` a `EPERM` (que es lo que dice XNU).

### 2.5 `TIOCFLUSH` y la velocidad del terminal — `emu_net.c`

* `TIOCFLUSH` (`_IOW('t',16,int)` = `0x80047410`) no estaba: `tcflush()` de la libc devolvía `ENOTTY`. Ahora está, y
  con la **semántica exacta de XNU** (`bsd/kern/tty.c:1155`):

  ```c
  case TIOCFLUSH: {
          int flags = *(int *)data;
          if (flags == 0) flags = FREAD | FWRITE;      /* 0 = las dos colas */
          else flags &= FREAD | FWRITE;
          ttyflush(tp, flags);
  ```

  Ojo: la libc de macOS pasa el selector de POSIX **tal cual** (desensamblado: `mov %esi,(%rdx)`, con
  `TCIFLUSH=0`, `TCIOFLUSH=1`, `TCOFLUSH=2`), así que con XNU `TCIFLUSH` (0) vacía **las dos** colas y `TCIOFLUSH` (1)
  vacía la de entrada. La primera versión de este cambio interpretaba el 0 como «solo la de entrada»; está corregido.
* La velocidad: en Darwin, `c_ispeed`/`c_ospeed` de `struct termios` son **los baudios**
  (`bsd/sys/termios.h`: `B9600 9600`, `B38400 38400`, …) y el emulador devolvía siempre 38400 (y al escribir
  devolvía la que había, sin traducir). Ahora se traducen con una tabla, aceptando las dos convenciones de Linux
  (los baudios de glibc o el código del núcleo en `CBAUD`), y si Linux no admite la velocidad se deja la que había
  (antes `cfsetospeed` con un valor inválido devolvía `EINVAL` y se ignoraba).

### 2.6 Limpieza: dos registros de la syscall 54

`emu_proc.c` registraba `reg_bsd(54, bsd_ioctl)` (un stub que devolvía `ENOTTY` siempre) y `emu_net.c`
`reg_bsd(54, net_ioctl)` (el de verdad). Gana el último que se registra, y `emu_net_init()` se llama después de
`emu_proc_init()`, así que funcionaba **por casualidad del orden de los `init`**. He borrado el stub muerto: si
alguien cambiara el orden, el `ioctl` entero se habría roto en silencio.

### 2.7 `setlogin`: el orden de las comprobaciones

XNU hace `proc_suser()` **antes** de `copyinstr`; un usuario sin privilegios recibe `EPERM` aunque además el nombre sea
demasiado largo. La primera versión comprobaba la cadena primero y devolvía `EINVAL`. Corregido.

### 2.8 Un manejador de SIGILL temprano — `tahoe_run.c`

Diagnóstico, no semántica: hasta `install_dispatch()` (línea 1459) el emulador **no atiende el SIGILL**. He instalado
un manejador simple justo después de preparar el descriptor del registro, que anota la dirección de la instrucción
ilegal y sale con 132 igual que el manejador completo. Con esto, la próxima vez que se caiga uno de estos programas
durante el arranque del emulador, en el registro habrá una línea
`SIGILL <pid> en el arranque del emulador, rip=0x…` (con `addr2line` se ve qué parte del emulador la ejecutó).

---

## 3. Lo que verifiqué (y cómo)

* **Semántica de `struct termios`**: contrastada con `bsd/sys/termios.h` de XNU 12377: `tcflag_t`/`speed_t` son
  `unsigned long` (8 bytes) y el tamaño total es **72 bytes**, que es justo lo que lleva el número de ioctl
  `TIOCGETA = 0x40487413` (el tamaño va en los bits 16-28 de `_IOC`). **La estructura y la traducción de banderas del
  emulador ya eran correctas** (`IMAP`/`OMAP`/`CMAP`/`LMAP` y el índice `CCMAP` de `c_cc`): lo comprobé una por una
  contra el encabezado y **no las he tocado**. En particular `IXON=0x200`, `IXOFF=0x400`, `IXANY=0x800` en Darwin (no
  hay `IUCLC`) y `ECHOKE=0x1 … IEXTEN=0x400` en las banderas locales.
* **Números de ioctl**: comprobados contra `_IOC` de `bsd/sys/ttycom.h` **y contra el código real de macOS 26**
  (desensamblando `libsystem_c`): `TIOCGETA/SETA/SETAW/SETAF` (`0x40487413`, `0x80487414`, `0x80487415`, `0x80487416`),
  `TIOCGWINSZ/SWINSZ` (`0x40087468`, `0x80087467`), `TIOCGPGRP/SPGRP` (`0x40047477`, `0x80047476`), `TIOCOUTQ`
  (`0x40047473`), `TIOCSTI` (`0x80017472`), `TIOCNOTTY` (`0x20007471`), `TIOCSCTTY` (`0x20007461`), `TIOCFLUSH`
  (`0x80047410`), `TIOCPTYGRANT/UNLK/GNAME` (`0x20007454`, `0x20007452`, `0x40807453`) y `FIODTYPE` (`0x4004667a`,
  `_IOR('f',122,int)`). En la libc de macOS: `isatty()` usa `ioctl(fd, FIODTYPE)` y comprueba que el tipo sea `D_TTY`
  (3) — el emulador responde `ENOTTY` a las dos cuando el descriptor no es un terminal ✓.
* **`ttyname()`**: desensamblado `libsystem_c.dylib`: `ttyname_r` hace `fstat` → `isatty` → exige `S_IFCHR` →
  `strlcpy(buf,"/dev/")` → `devname_r(st_rdev, S_IFCHR, …)`, y `devname_r` **recorre `/dev` con
  `opendir`/`readdir`/`stat` comparando `st_rdev` y el tipo**. El emulador ya presenta los `/dev/ttysNNN` con el
  `st_rdev` de macOS y con su entrada en el listado de `/dev` ✓.
* **Lo que necesita `login` y `screen`**: `screen` usa `ttyname`, `getlogin`, `getpwnam`, `isatty`, `ioctl`,
  `tcgetattr/tcsetattr`, `grantpt/unlockpt/ptsname`, `setsid`, `tcsetpgrp`; `login` usa `ttyname`, `getttynam`,
  `setlogin`, `auditon`, PAM y `getdtablesize`+`close` en bucle. De todo eso, lo que faltaba en el emulador eran las tres
  ioctl de pseudoterminal (§2.3) y `TIOCFLUSH` (§2.5).
* **El rastro de `login` en otra ejecución** (`~/.cache/tahoe-diag/msh.log`, 19-oct-2026, con traza de syscalls):
  `login` (pid 1676) arranca bien y hace exactamente lo que hace en un Mac: `ioctl(2, FIODTYPE)` → `ENOTTY`,
  `ioctl(2, TIOCGETA)` → `ENOTTY`, `ioctl(0, FIODTYPE)`/`TIOCGETA` → `ENOTTY` (no hay terminal), `getdtablesize` →
  **10240**, `read(0)` → 0 (fin de fichero) y `write(1, "login: ")` → 7 bytes. Es decir: **en esa ejecución `login` no se
  cayó**, y el emulador respondió bien a todo lo que `login` necesita por el camino. La caída del inventario (§4) no es
  por estas syscalls.

---

## 4. La lista de SIGILL: qué se puede deducir sin ejecutar

### 4.1 Los que aparecen en el inventario

De `~/.cache/tahoe-diag/inventario_total.txt` (el inventario de 1167 programas del macOS completo, 13-oct-2026), el
código de salida **132 = 128+4 = SIGILL** lo obtienen **12** programas:

| nº | programa | nº | programa |
|---|---|---|---|
| 442 | `/usr/bin/json_xs5.34` | 567 | `/usr/bin/net-snmp-create-v3-user` |
| 448 | `/usr/bin/kextutil` | 597 | `/usr/bin/package-stash-conflicts` |
| 451 | `/usr/bin/kgetcred` | 612 | `/usr/bin/pbcopy` |
| 452 | `/usr/bin/kill.d` | 713 | `/usr/bin/screen` |
| 455 | `/usr/bin/kmutil` | 1129 | `/usr/sbin/snmptrapd` |
| 496 | `/usr/bin/login` | 1161 | `/usr/sbin/vpnd` |

En el mismo inventario hay otras salidas por señal, que conviene no confundir: **134** (SIGABRT) en `jstack`,
`jstat`, `keytool`, `networkQuality`, `pack200`; **139** (SIGSEGV) en `keychain-access`; **255** (salida por error) en
varios; y `COLGADA` (el plazo de 6 s venció) en unos cuantos.

### 4.2 Qué es un SIGILL aquí, y por qué no hay rastro

En el emulador, el SIGILL casi siempre es un **`ud2` del código de macOS** (el `__builtin_trap()` de libsystem cuando
hay un error fatal): el manejador `on_sigill` (`tahoe_run.c`) imprime la dirección y sale con 132. En la caché de macOS
26 hay dos puntos de trampa:

| Dirección (x86_64h) | Biblioteca | Qué es |
|---|---|---|
| `0x7ffe0b5d4407` | `libimage4.dylib` | `ud2` tras leer el nodo de la plataforma; las funciones siguientes tienen las cadenas **«panic: failed to get platform node»** y **«panic: failed to get options node»** |
| `0x7ff812e880c3` | `libsystem_secinit.dylib` | `ud2` genérico de libSystem (trampa fatal) |

El primero lo dice todo: **libimage4** es la biblioteca que parsea las imágenes del núcleo y de los kexts, y hace
`panic` si no encuentra en el árbol de IOKit el nodo `IODeviceTree:/efi/platform` (o `/efi/options`). En el registro del
inventario aparece el mensaje: `abort <501>: [libsystem_c.dylib] "panic: failed to get platform node"`.

**Deducción sin ejecutar (grupo de kexts):** `kextutil` y `kmutil` son herramientas de kexts → usan libimage4 →
necesitan el nodo `IODeviceTree:/efi/platform` **con el contenido que libimage4 espera**. El `Pendientes.md` dice que ese
nodo ya se añadió (para que `kmutil` no abortara al arrancar), pero el inventario es del 13 de octubre y puede ser
anterior; y aunque el nodo exista, libimage4 **también** pide `/efi/options` (la otra cadena de panic). Es decir: la
caída de esas dos no es del emulador de syscalls sino de que **el árbol de IOKit que se publica no tiene (o no tiene
completo) el nodo de plataforma que libimage4 busca**; eso se puede mirar en la respuesta de
`IODeviceTree:/efi/platform` y `/efi/options` del emulador de IOKit (`emu_iokit.inc`).

### 4.3 El hallazgo importante: esas caídas no dejan rastro

En el registro del inventario (`~/.cache/tahoe-diag/inventario_total.log`, 114 MB) **no hay ninguna línea
`SIGILL <pid>`** de estos programas, ni siquiera de los que sí llegaron a cargarse (`screen`, `vpnd`, `snmptrapd` y
`pbcopy` tienen su línea `cargado …` pero no la de `SIGILL`). Y el emulador, cuando atiende el SIGILL, escribe siempre
esa línea antes de salir con 132.

Descartando las demás explicaciones (el `logfd` es un descriptor privado del registro y el aviso de `abort` tampoco
sale), la conclusión es:

> **La caída ocurre en el propio emulador, antes de que instale sus manejadores de señal** (`install_dispatch()`, línea
> 1459). Todo lo que pasa antes de esa línea —mapear la caché (`map_cache`, 1392), parchear el número de CPU
> (`patch_cpu_number`, 1393), el commpage (1395) y cargar las imágenes (1397-1398)— ocurre sin manejadores: un SIGILL
> ahí mata el proceso con 132 y **no deja ninguna línea en el registro**.

Encaja con los datos: en el registro **no aparece ninguna línea con el pid de estos doce programas** (para
`screen`, `vpnd`, `snmptrapd` y `pbcopy` sí hay líneas `cargado …`, o sea que llegaron a cargar la imagen, pero ninguna
`SIGILL`; para `login`, `kextutil`, `kgetcred`, `net-snmp-create-v3-user` y `kill.d` no hay ninguna línea con su pid, ni
siquiera de carga). Y como el emulador **sí** atiende el SIGILL cuando el que lo produce es el código de macOS (hay 85
procesos con línea
`SIGILL …` en el registro, con la dirección y la pila), lo que falla es el arranque del emulador, no el programa.

*Corrección importante frente a lo que se suponía al principio*: los 12 **no** caen en una ventana contigua del
inventario (van del 442 al 1161, con vecinos que salen bien: 0, 1, 2, 64, 134, 139, 255…), así que **no es una
condición transitoria del sistema** (memoria o una carrera puntual): es algo que ocurre en el arranque del emulador de
esos programas concretos. Queda por averiguar *qué* instrucción ilegal es; para eso está el manejador temprano (§2.8).

### 4.4 Lo que habría que mirar después (sin ejecutarlo aquí)

1. `map_cache()` mapea la caché en la dirección de XNU con `MAP_FIXED_NOREPLACE`; si algo del proceso ya ocupa esa
   dirección, el código reintenta sin ASLR (`TAHOE_REEXEC`). Un `mmap` a medias (una imagen sí, otra no) dejaría el dyld
   en un estado inconsistente.
2. `patch_cpu_number()` **escribe en el código de la caché** (cambia `sidt`/`sgdt` por `and dword [mem],0`) sobre la
   copia privada mapeada. Si el patrón aparece en otro sitio o el parche se solapa con otro, se cambia código
   ejecutable.
3. La caché con el *slide* se guarda en un archivo y se reutiliza (`slid_open`); si ese archivo quedó a medias de una
   ejecución anterior, el mapeo es de datos inconsistentes → el dyld ejecutaría basura → SIGILL.
4. `load_macho()` carga la imagen del programa; si el `st_mh` o el punto de entrada se leen mal, el salto a dyld
   puede acabar en un sitio inválido.

### 4.5 Los que no son de kexts

Para el resto (`vpnd`, `snmptrapd`, `net-snmp-create-v3-user`, `json_xs5.34`, `kill.d`, `package-stash-conflicts`,
`login`, `screen`, `pbcopy`) el punto de trampa es el genérico de libSystem: **no se puede saber qué condición lo
dispara** sin la dirección de la trampa, y el emulador no la estaba guardando (§2.8, §4.3). Con lo puesto, la próxima
vuelta del inventario dirá si es un `ud2` del invitado (dirección en la caché de dyld) o una instrucción del propio
emulador (dirección en el binario de tahoe-run, legible con `addr2line`).

Lo que sí se puede decir de cada grupo, por lo que usan:

| grupo | programas | qué usan | qué se puede deducir |
|---|---|---|---|
| kexts | `kextutil`, `kmutil` | libimage4 | el `panic` de «failed to get platform node» está en el registro; falta (o está incompleto) el nodo de IOKit |
| scripts de Perl | `json_xs5.34`, `kill.d`, `net-snmp-create-v3-user`, `package-stash-conflicts` | Perl + módulos XS | en el registro, los Perl nearby (`perlthanks`, `cpan`, `perlivp`) salen bien o se quedan colgados: no es un problema de Perl |
| red/daemons | `vpnd`, `snmptrapd` | red, y **cierran todos sus descriptores** al arrancar (el propio repo lo anota en `tahoe.h`: «snmptrapd, vpnd: close desde getdtablesize hacia abajo») | el emulador protege sus descriptores internos (`bsd_close` devuelve `EBADF` a partir de `FD_INTERNAL_BASE`), pero un `close` masivo puede dejar al proceso sin terminal ni con los descriptores que necesita |
| seguridad | `kgetcred`, `login` | Kerberos / PAM → Security.framework | en el registro hay 13 caídas de libsystem (`terminating due to uncaught exception of type Security::MachPlusPlus::Error`) y 19 de `libsystem_secinit` («Incoming message euid:205 does not match secinitd uid:0»): la ruta de Security sí tiene problemas |
| pty | `screen` | pseudoterminal | le faltaban las tres ioctl de pty (§2.3) y, con ellas, el desbloqueo del esclavo |
| portapapeles | `pbcopy` | XPC (necesita sesión gráfica) | en el registro, `pbcopy` pide `necp_open` (syscall 501, sin implementar: 627 llamadas en toda la ejecución) y recibe un `SIGSEGV` en libsystem |

---

## 5. Qué compiló y qué no

**Compilado** (con `nice -n 19 ionice -c3`, un solo compilador a la vez):

* `cargo build --release -j 1 --manifest-path tahoe-run/nucleo/Cargo.toml` → correcto (no he tocado el Rust).
* `clang -O1 -g -Wall -Wextra -Wno-unused-parameter -static` de los 15 ficheros de `tahoe-run/` → **sin errores ni
  avisos nuevos** (solo el aviso de siempre de `getpwuid` con enlace estático). Binario nuevo en `/tmp/tahoe-run-w3`
  (3.774.408 bytes). **No** he sustituido `tahoe-run/tahoe-run` del repositorio (está en `.gitignore`); para
  reconstruirlo: `tahoe-run/build.sh ~/macos26/src/xnu`.
* Un programa de prueba propio (`/tmp/opencode/pty_test.c`, `/tmp/opencode/pty3.c`) para comprobar que `TIOCGPTN` da el
  índice del pty y que el esclavo está bloqueado hasta hacer `TIOCSPTLCK(0)` (sin eso, `open` da `EIO`). No es el
  emulador, solo una comprobación del supuesto en el que se basa §2.3.

**NO verificado (no he podido ejecutarlo):**

* Que `login` y `screen` dejen de caerse: **no lo sé**. Lo que he corregido son desviaciones reales de la semántica de
  XNU en la zona que usan (las ioctl de pty, `TIOCFLUSH`, `getlogin`/`setlogin`, `TIOCSCTTY`), pero la caída que se
  observa parece estar **en el arranque del emulador** (§4.3), que es otra cosa. **No lo he probado.**
* Que las ioctl nuevas de pseudoterminal hagan que `screen` cree bien su pty: la traducción `/dev/ttysNNN` ↔
  `/dev/pts/N` ya existía y es coherente con el listado de `/dev`, y el desbloqueo del esclavo está comprobado con un
  programa de prueba propio, pero **no lo he probado con `screen`**.
* Que `getlogin`/`setlogin` con la semántica nueva no rompan a quien los usaba: los servicios que llaman a `getlogin()`
  siguen viendo `root` (el nombre solo cambia si alguien hace `setlogin`).
* Que la velocidad nueva del terminal no rompa nada: depende de la convención que use glibc para
  `cfgetospeed`/`cfsetospeed`, que **no he podido comprobar** sin ejecutar un programa de prueba; por eso la tabla
  acepta las dos convenciones y, si `cfsetospeed` falla, se conserva la velocidad anterior.
* El nombre de la sesión (`setlogin`) **no se comparte entre procesos** de macOS (tabla por proceso de tahoe-run) ni
  sobrevive a un `exec` (§2.2).
* **El diagnóstico del §4.3 no está confirmado**: que la caída sea un SIGILL del emulador antes de `install_dispatch()`
  es la explicación que encaja con la ausencia de la línea `SIGILL <pid>` en el registro, pero **no lo he visto
  ocurrir**. El manejador temprano (§2.8) lo confirmará (o lo refutará) en la próxima ejecución.
* No he ejecutado `cargo test --lib` (la batería) ni el inventario; los datos sobre los SIGILL salen de leer el
  inventario y los registros que ya había en `~/.cache/tahoe-diag/`.

---

## 6. Ficheros tocados

* `tahoe-run/emu_net.c` — ioctl de terminal: `TIOCSCTTY` según XNU, `TIOCFLUSH` con la semántica de XNU, las tres
  `TIOCPTY*` (con desbloqueo del esclavo y `ENOTTY`), y la velocidad de `struct termios`.
* `tahoe-run/tahoe_run.c` — `getlogin`/`setlogin` según XNU (con el nombre de la sesión), registro de la syscall 50 y el
  manejador de SIGILL temprano.
* `tahoe-run/emu_proc.c` — fuera el registro erróneo de la syscall 50 y el stub muerto de `ioctl`.

Diff completo en `../parche-3.diff` (252 líneas, 3 ficheros).
