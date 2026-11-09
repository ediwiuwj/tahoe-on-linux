// Prueba invitada Mach-O: sin SDK ni archivos Apple en CI.
// Declaraciones ABI Darwin LP64; sólo prueba, no forma parte del emulador.
typedef unsigned int sigset_t;
struct sigaction_d { void (*handler)(int, void*, void*); sigset_t mask; int flags; };
int sigaction(int, const struct sigaction_d*, struct sigaction_d*);
int sigprocmask(int, const sigset_t*, sigset_t*);
int sigpending(sigset_t*);
int kill(int, int);
int getpid(void);
int printf(const char*, ...);
int write(int, const void*, unsigned long);
static volatile int uno, dos, fallo_info, cruzar, externa_int;
static int pid_esperado, uid_esperado;
static int decimal(const char* s) { int n=0; while (*s>='0'&&*s<='9') n=n*10+(*s++-'0'); return n; }
static void recibir(int signo, void* info, void* contexto) {
    const int* campos = info;
    if (!info || !contexto || campos[0] != signo) { fallo_info=1; return; }
    if (signo == 2) {
        if (campos[2] != 0x10001 || campos[3] != pid_esperado || campos[4] != uid_esperado) fallo_info=1;
        externa_int++;
    } else if (campos[2] != 0x10001) fallo_info=1;
    if (signo == 30) {
        uno++;
        if (cruzar && kill(getpid(), 31) != 0) fallo_info = 1;
    } else if (signo == 31) dos++;
    else if (signo != 2) fallo_info=1;
}
#define EXIGIR(x) do { if (!(x)) { printf("FALLO linea %d\n", __LINE__); return 1; } } while (0)
int main(int argc, char** argv) {
    if (argc > 1 && argv[1][0]=='e') {
        if (argc < 4) return 9;
        pid_esperado=decimal(argv[2]); uid_esperado=decimal(argv[3]);
        struct sigaction_d int_action = { recibir, 0, 0x40 };
        if (sigaction(2,&int_action,0)) return 10;
        if (write(1,"SIGINT_LISTA\\n",13) != 13) return 13;
        for (int i=0; i<1000000 && !externa_int; i++) if (kill(getpid(),0)) return 11;
        if (externa_int!=1 || fallo_info) return 12;
        printf("SIGINT_EXTERNA_INFO_OK\\n"); return 0;
    }
    struct sigaction_d action = { recibir, 0, 0x40 };
    EXIGIR(sigaction(30, &action, 0) == 0);
    EXIGIR(sigaction(31, &action, 0) == 0);
    sigset_t bloqueadas = (1u << 29) | (1u << 30), original = 0, pendientes = 0;
    EXIGIR(sigprocmask(1, &bloqueadas, &original) == 0);
    EXIGIR(kill(getpid(), 30) == 0);
    EXIGIR(kill(getpid(), 31) == 0);
    EXIGIR(uno == 0 && dos == 0);
    EXIGIR(sigpending(&pendientes) == 0);
    EXIGIR((pendientes & bloqueadas) == bloqueadas);
    sigset_t bit = 1u << 30;
    EXIGIR(sigprocmask(2, &bit, 0) == 0);
    EXIGIR(uno == 0 && dos == 1);
    EXIGIR(sigpending(&pendientes) == 0);
    EXIGIR((pendientes & bloqueadas) == (1u << 29));
    bit = 1u << 29;
    EXIGIR(sigprocmask(2, &bit, 0) == 0);
    EXIGIR(uno == 1 && dos == 1 && !fallo_info);
    EXIGIR(sigpending(&pendientes) == 0);
    EXIGIR((pendientes & bloqueadas) == 0);
    EXIGIR(sigprocmask(3, &original, 0) == 0);
    cruzar = 1;
    EXIGIR(kill(getpid(), 30) == 0);
    EXIGIR(uno == 2 && dos == 2 && !fallo_info);
    printf("SENALES_MASCARAS_SIGINFO_OK\n");
    return 0;
}
