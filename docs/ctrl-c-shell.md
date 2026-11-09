# Ctrl-C en el prompt: SIGINT diferida

Estado: compilado en Actions `42590955503`, commit
`2c38cdae4767efffb0a21a3ffe0adfde33aaafc7`, y probado con Recovery 26.6.2.
Pasaron Ctrl-C en el prompt (1 y 10 veces), foreground (1 y 10 veces) y
Ctrl-Z/jobs/bg/fg. No equivale a soporte completo de señales ni grupos.

## Entrega

Alcance monohilo: INT Darwin 2 y USR Darwin 30/31. La selección Rust segura
incluye ahora el bit 1 y mantiene prioridad determinista INT, USR1, USR2, filtrada
por máscara y señales permitidas. Las pruebas cubren combinaciones de las tres,
más bits no soportados; no se ha añadido Rust unsafe ni cambiado la FFI.

El colector conserva slots TLS initial-exec y atomics de 64 bits lock-free.
USR1/USR2 mantienen SI_USER. INT admite SI_USER con pid positivo y SI_KERNEL
(del terminal); no admite sigqueue/tgkill como si fueran kill. Los slots
coalescen: gana el primer origen hasta publicar el marco o descartar por IGN.

Para INT SI_KERNEL se guarda sólo un marcador, sin leer la unión pid/uid Linux.
El siginfo Darwin lleva signo 2, código 0 (sin código específico) y pid/uid 0;
no se etiqueta SI_USER ni se inventa un remitente. Darwin no define SI_KERNEL
en [sys/signal.h de XNU](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/signal.h).
SI_USER conserva código Darwin 0x10001 y pid/uid del emisor. El marcador no
colisiona con SI_USER de INT porque este último exige pid positivo.

Captura, IGN y DFL se instalan mediante el restaurador propio existente;
DFL conserva la acción Linux predeterminada. sigpending, limpieza de hilo y
fork incluyen el nuevo slot. Self-kill INT utiliza el pendiente Darwin como
USR, sin consumir una llegada externa simultánea. No se amplía entrega a grupos
ni selección multihilo.

El handler guest sólo se ejecuta mediante el contexto del epílogo SIGSYS,
después de materializar el retorno de syscall. No hay llamada directa desde C.
No se altera la instalación ni máscara de infraestructura SIGSYS. El colector
bloquea las tres señales únicamente durante su propia ejecución, sin NODEFER
ni SA_RESTART; no se añade un bloqueo permanente de INT.

## Saltos no locales

Se reutilizan sin refactor los marcos transaccionales y la política Rust de
solapamiento de `5e39404`; véase [reuso-marcos-senal.md](reuso-marcos-senal.md).
Según el coordinador, su prueba en run `42589774841` produjo
`SIGLONGJMP_REPETIDO_OK veces=24 internas=12`. Es validación del prerrequisito,
no de SIGINT ni del parche actual. No se vacían todos los marcos en RESET ni se
infiere abandono comparando simplemente RSP. Los límites de reutilización y
solapamiento parcial documentados allí siguen vigentes para Bash.

## Límites

`read` ya traduce EINTR Linux a Darwin antes del epílogo. No se reinicia
artificialmente para esconder Ctrl-C. Aunque se acepta el flag Darwin
SA_RESTART, este incremento no implementa reinicio de syscalls interrumpidas.
Esperas que reintentan EINTR internamente podrían impedir llegar al epílogo.
Una señal recibida fuera de una syscall puede esperar hasta la siguiente salida
SIGSYS; existe una ventana entre el último epílogo y una espera bloqueante.
Bucles sin syscalls no reciben el handler inmediatamente. No se garantiza
latencia ni wakeup atómico. Las pruebas foreground y bg/fg listadas arriba
pasaron, pero no cubren la gestión completa de grupos ni todas las carreras.

## Validación pendiente

1. Compilar exclusivamente en Actions; registrar SHA, run y artefacto exactos.
2. Ejecutar PTY `--ctrl-c`: prompt inicial, Ctrl-C, nuevo prompt, mismo Bash vivo
   y comando marcador posterior. No usar el binario baseline como prueba.
3. Sólo tras ese pase, diez Ctrl-C sincronizados con cada prompt, comprobando
   comando marcador tras cada uno, sin EAGAIN, salida ni bloqueo.
4. Repetir USR externas, metadata, máscaras, contextos, anidamiento y retorno,
   además del signaljumpprobe. Probar INT SI_USER con pid/uid reales, INT de
   terminal sin remitente, IGN, DFL y pendiente bloqueada/desbloqueada.
5. Mantener visible el fallo cruzado de trap de Bash 3.2 previamente documentado;
   no reclasificarlo como pase.

Las pruebas compiladas y PTY indicadas en la cabecera se ejecutaron localmente
con el artefacto de Actions, sin compilación local. Quedan pendientes los casos
adicionales de metadata INT, IGN/DFL y todas las carreras de esperas descritas.
