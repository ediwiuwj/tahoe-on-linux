// Prueba Mach-O de la frontera posix_spawn; no incluye SDK de Apple.
typedef void* acciones_t;
int posix_spawn_file_actions_init(acciones_t*);
int posix_spawn_file_actions_destroy(acciones_t*);
int posix_spawn_file_actions_adddup2(acciones_t*, int, int);
int posix_spawn_file_actions_addopen(acciones_t*, int, const char*, int, unsigned short);
int posix_spawnattr_init(void*);
int posix_spawnattr_destroy(void*);
int posix_spawnattr_setflags(void*, short);
int posix_spawn(int*, const char*, const acciones_t*, const void*, char* const*, char* const*);
int open(const char*, int, ...);
int close(int);
int write(int, const void*, unsigned long);
int getdtablesize(void);
int waitpid(int, int*, int);
int printf(const char*, ...);
#define EXIGIR(x) do { if (!(x)) { printf("FALLO spawn linea %d\n", __LINE__); return 1; } } while (0)
int main(void) {
    acciones_t acciones;
    EXIGIR(posix_spawn_file_actions_init(&acciones) == 0);
    char* argv[] = { "/usr/bin/true", 0 };
    char* envp[] = { "PATH=/usr/bin:/bin", 0 };
    int pid = 0, status = -1;
    EXIGIR(posix_spawn_file_actions_adddup2(&acciones, 1, 1) == 0);
    EXIGIR(posix_spawn(&pid, argv[0], &acciones, 0, argv, envp) == 0);
    EXIGIR(waitpid(pid, &status, 0) == pid && status == 0);
    for (int i = 1; i < 33; i++)
        EXIGIR(posix_spawn_file_actions_adddup2(&acciones, 1, 1) == 0);
    int error = posix_spawn(&pid, argv[0], &acciones, 0, argv, envp);
    if (error == 0) waitpid(pid, &status, 0);
    EXIGIR(posix_spawn_file_actions_destroy(&acciones) == 0);
    EXIGIR(error == 7); // Límite explícito E2BIG, nunca éxito con acciones truncadas.
    int high = open("/dev/null", 0);
    while (high >= 0 && high <= 1025) high = open("/dev/null", 0);
    EXIGIR(high > 1025 && high < 10240);
    acciones_t altas;
    EXIGIR(posix_spawn_file_actions_init(&altas) == 0);
    EXIGIR(posix_spawn_file_actions_adddup2(&altas, high, 1025) == 0);
    EXIGIR(posix_spawn_file_actions_addopen(&altas, 1, "/dev/null", 1, 0) == 0);
    void* attr = 0;
    EXIGIR(posix_spawnattr_init(&attr) == 0);
    EXIGIR(posix_spawnattr_setflags(&attr, 0x4000) == 0); // CLOEXEC_DEFAULT
    char* test_high[] = { "/bin/sh", "-c", "printf high >&1025", 0 };
    int highpid = 0, error_high = posix_spawn(&highpid, test_high[0], &altas, &attr, test_high, envp);
    status = -1;
    if (!error_high) waitpid(highpid, &status, 0);
    EXIGIR(posix_spawnattr_destroy(&attr) == 0);
    EXIGIR(posix_spawn_file_actions_destroy(&altas) == 0);
    for (int fd = 3; fd <= high; fd++) close(fd);
    EXIGIR(error_high == 0 && status == 0);
    printf("SPAWN_FD_ALTO_OK\\n");
    printf("SPAWN_SIN_TRUNCAR_OK\\n");
    return 0;
}
