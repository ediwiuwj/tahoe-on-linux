// Interfaz común entre el núcleo de tahoe-run y los módulos de emulación (emu_*.c).
#pragma once
#include <unistd.h>
#include <stdint.h>
#include <stddef.h>
#include <sys/types.h>
#include <ucontext.h>

#define PAGE 4096UL
static inline uint64_t round_up(uint64_t v) { return (v + PAGE - 1) & ~(PAGE - 1); }

#define CACHE_BASE 0x00007ff800000000UL
#define STACK_TOP  0x00007ff7bff00000UL

// Contexto de una syscall interceptada: número y hasta 6 argumentos (registros rdi, rsi, rdx, r10, r8, r9).
struct ctx { uint64_t nr, a[6]; ucontext_t* uc; uint64_t ret2; int has_ret2; int raw_ret; };   // raw_ret: devolver r tal cual (sin CF), p. ej. ULF_NO_ERRNO   // ret2 -> rdx (fork, pipe)
typedef long (*emu_fn)(struct ctx*);

uint64_t ctx_arg(struct ctx* c, int i);   // argumento i (0..7); a partir del 6.º va en la pila

// BSD: devuelve >= 0, o -errno_de_Darwin. Mach: devuelve un kern_return_t.
void reg_bsd(unsigned num, emu_fn fn);
void reg_mach(unsigned num, emu_fn fn);

// Utilidades
void logf_(const char* fmt, ...);
int darwin_errno(int linux_errno);
int darwin_open_flags(int darwin_flags);
long open_symlink(int dfd, const char* p, int dflags, int lflags, long fd);   // O_SYMLINK de Darwin
ssize_t safe_read(uint64_t addr, void* buf, size_t len);
ssize_t safe_write(uint64_t addr, const void* buf, size_t len);
int safe_string(uint64_t addr, char* out, size_t max);
#include <ucontext.h>
void diag_crash(ucontext_t* uc);
void diag_stack(ucontext_t* uc, const char* what);   // "abort" (caída) o "espera" (bloqueo, STACKS)
extern const char* tahoe_root;
extern char cache_guest_path[1024];   // ruta de la caché vista desde macOS (sin la raíz del host)
extern uint64_t cache_ino;
void dsmos_unprotect(uint8_t* seg, uint64_t fileoff, uint64_t filesize, const char* path);   // emu_dsmos.c
void sys_volumes_mount(void);       // emu_fs.c: volúmenes Data y Preboot de un macOS completo (en el PID 1)
extern const char *g_cache_path, *g_dyld_path, *g_self_exe;   // para relanzar tahoe-run en execve   // raíz del sistema de archivos de macOS 26 (TAHOE_ROOT)
extern int trace_all;

// Módulos
void emu_sysctl_init(void);
void emu_fileport_init(void);
int exe_host_path(int pid, char* out, size_t cap);
void data_log_storage(const char* data_root);   // almacén de logd en un volumen de datos (emu_fs.c)
int proc_dir(int pid, char* out, size_t cap);
int exe_uuid(int pid, uint8_t out[16]);          // LC_UUID del ejecutable de macOS del proceso   // /proc/<pid>, o el de un hilo vivo si el líder es zombi
int darwin_sig_to_linux(int s);                         // número de señal de Darwin -> Linux (0 si no hay)   // ruta en el anfitrión del ejecutable de macOS de un proceso
void emu_fs_init(void);
void emu_mach_init(void);
void emu_proc_init(void);
void emu_kqueue_init(void);
void emu_net_init(void);
void emu_fs2_init(void);
void emu_port_init(void);
uint32_t port_create(int is_set);
// Códigos de mach_msg (osfmk/mach/message.h). Con sufijo _ para no chocar con definiciones locales.
#define MACH_SEND_INVALID_DATA_  0x10000002
#define MACH_SEND_INVALID_DEST_  0x10000003
#define MACH_RCV_INVALID_NAME_   0x10004002
#define MACH_RCV_TIMED_OUT_      0x10004003
#define MACH_RCV_TOO_LARGE__     0x10004004
#define MACH_RCV_INVALID_DATA_   0x10004008
#define MACH_RCV_PORT_DIED_      0x10004009
int port_exists(uint32_t name);
static inline int mach_trace(void) { static int v = -1; if (v < 0) v = access("/dev/shm/tahoe-mach-trace", F_OK) == 0; return v; }
// solo los workloops (peticiones de hilo, entregas, EVFILT_WORKLOOP): mucho menos que la traza Mach, para carreras que esta
// tapa al cambiar los tiempos (msh.sh WL=1). La traza Mach completa la incluye.
static inline int wl_trace_on(void) { static int v = -1; if (v < 0) v = mach_trace() || access("/dev/shm/tahoe-wl-trace", F_OK) == 0; return v; }
int port_rpid(uint32_t name);
int port_list_owned(uint32_t* names, uint32_t* types, int max);
uint64_t port_get_context(uint32_t name);
int port_set_context(uint32_t name, uint64_t v);   // kern_return_t
int port_guard(uint32_t name, uint64_t guard, int strict);
int port_unguard(uint32_t name, uint64_t guard);
extern __thread int g_watch;
int port_drain_idle(uint32_t name);
int diag_target(void);
int diag_all(void);
int msg_trace(void);
int trace_file_on(const char* file);
void diag_dump_threads(void);
void diag_breakpoints(void);
uint32_t cred_euid(void);
uint32_t cred_egid(void);
uint32_t cred_auid(void);           // emu_cred.c: sesión de auditoría del proceso (auid, asid)
uint32_t cred_asid(void);
void cred_env(char* out, size_t cap);
void emu_cred_init(void);
void emu_fs_xattr_init(void);
int tahoe_host_passthrough(const char* p);
const char* tahoe_dev_alias(const char* p, char* buf, size_t cap);
void memfd_reopen(void);
int fsevents_open(void);
void ports_forked_child(void);
void fileport_forked_child(void);
uint32_t guest_uid(uint32_t u);
uint32_t darwin_dev(unsigned ma, unsigned mi);
uint32_t guest_gid(uint32_t g);
#include <signal.h>
int tahoe_sigaction(int sig, void (*h)(int, siginfo_t*, void*));
int port_eventfd(uint32_t name);
int port_send(uint32_t dest, const uint8_t* msg, uint32_t size);
int port_send_timeout(uint32_t dest, const uint8_t* msg, uint32_t size, int timeout_ms);   // mach_msg con MACH_SEND_TIMEOUT
int port_receive(uint32_t name, int timeout_ms, uint8_t** out, uint32_t* size, uint32_t* sender, uint32_t limit, int large);
void emu_sem_init(void);
void emu_psynch_init(void);
void emu_cs_init(void);
uint32_t sem_create(int value);
int machsem_destroy(uint32_t name);
uint32_t alloc_port(void);
uint32_t tahoe_flags_fd(int fd);
uint32_t tahoe_flags_path(const char* p);
void port_move_receive(uint32_t name);
int port_release_send_once(uint32_t name);
int port_assert_guard(uint32_t name, uint64_t guard);
int port_swap_guard(uint32_t name, uint64_t old, uint64_t nw);
int port_get_refs(uint32_t name, uint32_t right, uint32_t* refs);
enum { KO_TASK = 1, KO_TASK_NAME = 2, KO_IDENTITY_TOKEN = 3, KO_FILEPORT = 4, KO_THREAD = 5 };   // KO_THREAD: pid = tid de Linux
uint32_t kobj_make(int type, int32_t pid);
uint32_t kobj_new(int type, int32_t pid);
void kobj_free(uint32_t name);
int kobj_lookup(uint32_t name, int* type, int32_t* pid);
uint32_t execwatch_add(int32_t target);   // NOTE_EXEC: puerto que recibe un mensaje cuando target hace exec
void execwatch_del(uint32_t port);
void execwatch_notify(void);
uint32_t vfswatch_add(void);                  // EVFILT_FS: puerto que recibe los eventos de VFS
void vfs_event_signal(uint32_t vq);         // VQ_MOUNT/VQ_UNMOUNT/VQ_UPDATE a quien vigila             // antes de exec: avisar a quien vigila este proceso
uint64_t rmap_add(int32_t pid, const char* path, uint64_t off, uint64_t len, int prot, uint64_t align);
int rmap_fault(uint64_t addr);
int port_get_attributes(uint32_t name, int flavor, uint32_t* out, uint32_t* cnt);
int port_set_attributes(uint32_t name, int flavor, const uint32_t* in, uint32_t cnt);
int port_request_notification(uint32_t name, int variant, uint32_t sync, uint32_t notify, uint32_t* previous);
long mach_rx_message(uint32_t rcv_name, int timeout_ms, uint64_t options, uint64_t buf, uint32_t cap, uint32_t* total);
uint32_t special_port_get(int which);
void registered_ports_set(const uint32_t* p, int n);
int ports_inherit_env(char out[][40], int max);
uint64_t osvariant_status_get(void);
int osvariant_status_set(uint64_t v);
void special_port_set(int which, uint32_t name);
void reenable_dispatch(void);   // el kernel no hereda la interceptación de syscalls en el hijo de un fork

// Códigos de error de Darwin
enum { D_EPERM = 1, D_ENOENT = 2, D_EBADF = 9, D_ENOMEM = 12, D_EFAULT = 14, D_EINVAL = 22, D_ENOSYS = 78 };
