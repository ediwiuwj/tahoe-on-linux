// Prueba de ABI Mach original; sólo lectura de estado y objetos temporales propios.
typedef unsigned port_t;
extern port_t mach_task_self_;
int mach_port_allocate(port_t,int,port_t*);
int mach_port_insert_member(port_t,port_t,port_t);
int mach_port_extract_member(port_t,port_t,port_t);
int mach_port_guard(port_t,port_t,unsigned long long,int);
int mach_port_unguard(port_t,port_t,unsigned long long);
int mach_port_destroy(port_t,port_t);
int semaphore_create(port_t,port_t*,int,int);
int semaphore_destroy(port_t,port_t);
int semaphore_wait(port_t);
int semaphore_signal(port_t);
int semaphore_signal_all(port_t);
struct tiempo { unsigned sec,nsec; };
int semaphore_timedwait(port_t,struct tiempo);
int printf(const char*,...);
#define EXIGIR(x) do { if (!(x)) { printf("MACH_FALLO linea=%d\n",__LINE__); return 1; } } while(0)
int main(void) {
    port_t task=mach_task_self_, p=0, set=0, sem=0;
    EXIGIR(mach_port_allocate(task,1,&p)==0);
    EXIGIR(mach_port_allocate(task,3,&set)==0);
    EXIGIR(mach_port_insert_member(task,p,set)==0);
    EXIGIR(mach_port_insert_member(task,p,set)==17);
    EXIGIR(mach_port_extract_member(task,p,set)==0);
    EXIGIR(mach_port_extract_member(task,p,set)==18);
    EXIGIR(mach_port_guard(task,p,0x12345678ULL,0)==0);
    EXIGIR(mach_port_unguard(task,p,0x12345678ULL)==0);
    EXIGIR(semaphore_create(task,&sem,0,1)==0 && sem!=0);
    EXIGIR(semaphore_wait(sem)==0);
    struct tiempo cero={0,0};
    EXIGIR(semaphore_timedwait(sem,cero)==49);
    EXIGIR(semaphore_signal(sem)==0);
    EXIGIR(semaphore_timedwait(sem,cero)==0);
    EXIGIR(semaphore_signal_all(sem)==0);
    EXIGIR(semaphore_timedwait(sem,cero)==49);
    EXIGIR(semaphore_destroy(task,sem)==0);
    EXIGIR(mach_port_destroy(task,p)==0);
    EXIGIR(mach_port_destroy(task,set)==0);
    printf("MACH_PUERTOS_SEMAFOROS_OK\n");
    return 0;
}
