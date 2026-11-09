// Regresión Mach-O de siglongjmp fuera del handler, sin SDK Apple.
// Buffer opaco sobredimensionado y alineado: libplatform x86_64 usa offsets
// verificados hasta JB_ONSTACK=88 en la revisión documentada.
int sigsetjmp(void*, int) __attribute__((returns_twice));
void siglongjmp(void*, int) __attribute__((noreturn));
struct action { void (*handler)(int, void*, void*); unsigned mask; int flags; };
int sigaction(int, const struct action*, struct action*);
int sigprocmask(int, const unsigned*, unsigned*);
int kill(int, int);
int getpid(void);
int printf(const char*, ...);
static _Alignas(64) unsigned char salto[4096];
static volatile int veces;
static void recibir(int signo, void* info, void* contexto) {
    (void)info; (void)contexto;
    if (signo == 30) { veces++; siglongjmp(salto, 7); }
}
int main(void) {
    struct action a = { recibir, 0, 0x40 };
    if (sigaction(30, &a, 0)) return 1;
    while (veces < 12) {
        int vuelta = sigsetjmp(salto, 1);
        if (!vuelta) {
            int error = kill(getpid(), 30);
            printf("SALTO_NO_REALIZADO veces=%d kill=%d\n", veces, error);
            return 2;
        }
        if (vuelta != 7) return 3;
        unsigned mask = 0;
        if (sigprocmask(1, 0, &mask) || (mask & (1u << 29))) return 4;
    }
    printf("SIGLONGJMP_REPETIDO_OK veces=%d\n", veces);
    return 0;
}
