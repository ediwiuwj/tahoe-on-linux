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
static _Alignas(64) unsigned char salto[4096], salto_interno[4096];
static volatile int veces, modo, internas, fallo;
static void recibir(int signo, void* info, void* contexto) {
    (void)info; (void)contexto;
    if (signo == 31 && modo == 2) { internas++; siglongjmp(salto_interno, 9); }
    if (signo == 30) {
        veces++;
        if (modo == 2) {
            int vuelta = sigsetjmp(salto_interno, 1);
            if (!vuelta) { if (kill(getpid(), 31)) fallo = 1; }
            else if (vuelta != 9) fallo = 1;
            return; // el handler exterior debe conservar su sigreturn válido
        }
        siglongjmp(salto, 7);
    }
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
    modo = 2;
    if (sigaction(31, &a, 0)) return 5;
    for (int i = 0; i < 12; i++) {
        if (kill(getpid(), 30)) return 6;
        if (fallo || internas != i + 1) return 7;
    }
    if (veces != 24) return 8;
    printf("SIGLONGJMP_REPETIDO_OK veces=%d internas=%d\n", veces, internas);
    return 0;
}
