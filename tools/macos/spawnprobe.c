// Prueba Mach-O de la frontera posix_spawn; no incluye SDK de Apple.
typedef void* acciones_t;
int posix_spawn_file_actions_init(acciones_t*);
int posix_spawn_file_actions_destroy(acciones_t*);
int posix_spawn_file_actions_adddup2(acciones_t*, int, int);
int posix_spawn(int*, const char*, const acciones_t*, const void*, char* const*, char* const*);
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
    printf("SPAWN_SIN_TRUNCAR_OK\n");
    return 0;
}
