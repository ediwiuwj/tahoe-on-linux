// fileports: descriptores de archivo convertidos en puertos Mach (bsd/kern/kern_descrip.c de XNU). Así se pasan archivos y
// sockets entre procesos de macOS: launchd entrega a cada servicio los sockets de su plist, logd entrega su almacén de
// registros a `log`, libxpc los manda dentro de los mensajes (xpc_fd_create / xpc_fd_dup).
//
//   fileport_makeport(fd, *nombre)  (430): un derecho de envío a un objeto del núcleo que mantiene vivo el archivo aunque
//                                          se cierre el descriptor (fg_ref). EBADF si fd no es válido.
//   fileport_makefd(nombre)         (431): un descriptor nuevo del mismo archivo, con close-on-exec (FP_CLOEXEC). EINVAL si
//                                          el nombre no es un fileport.
//
// Aquí el objeto es un kobj KO_FILEPORT de la región compartida con el pid de su dueño, que guarda un duplicado del
// descriptor. Si lo pide otro proceso, el dueño se lo pasa con SCM_RIGHTS por un socket Unix abstracto (lo nativo de Linux
// para pasar descriptores entre procesos), de modo que funciona igual con archivos, directorios y sockets.
#define _GNU_SOURCE
#include <errno.h>
#include <stddef.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

#include "tahoe.h"

#define FP_MAX 512
#define FP_FD_MIN FD_INTERNAL_BASE   // duplicados fuera del rango que usan los programas (ver fd_hi)
static struct { uint32_t name; int fd; } fp_tab[FP_MAX];
static int fp_next;             // reemplazo circular si la tabla se llena (el más antiguo)
static pthread_mutex_t fp_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_once_t fp_srv_once = PTHREAD_ONCE_INIT;

static void fp_sockname(struct sockaddr_un* a, socklen_t* len, int pid) {
	memset(a, 0, sizeof *a);
	a->sun_family = AF_UNIX;
	const char* tag = getenv("TAHOE_PORTS_OWNER");
	// snprintf devuelve la longitud que habría escrito, mayor que el búfer si trunca: si el nombre no cabe, el
	// longitud apuntaría fuera de sun_path y el par se leería más allá del búfer.
	int n = snprintf(a->sun_path + 1, sizeof a->sun_path - 1, "tahoe.%s.fp.%d", tag ? tag : "x", pid);
	if (n < 0) n = 0;
	if ((size_t)n >= sizeof a->sun_path - 1) n = (int)(sizeof a->sun_path - 2);
	*len = (socklen_t)(offsetof(struct sockaddr_un, sun_path) + 1 + n);
}
static int fp_local(uint32_t name) {
	int fd = -1;
	pthread_mutex_lock(&fp_lock);
	for (int i = 0; i < FP_MAX; i++) if (fp_tab[i].name == name) { fd = fp_tab[i].fd; break; }
	pthread_mutex_unlock(&fp_lock);
	return fd;
}
static int send_fd(int s, int fd) {
	char ok = fd >= 0;
	struct iovec io = { &ok, 1 };
	union { struct cmsghdr h; char b[CMSG_SPACE(sizeof(int))]; } cm;
	struct msghdr m = { .msg_iov = &io, .msg_iovlen = 1 };
	if (fd >= 0) {
		m.msg_control = cm.b; m.msg_controllen = sizeof cm.b;
		struct cmsghdr* h = CMSG_FIRSTHDR(&m);
		h->cmsg_level = SOL_SOCKET; h->cmsg_type = SCM_RIGHTS; h->cmsg_len = CMSG_LEN(sizeof(int));
		memcpy(CMSG_DATA(h), &fd, sizeof(int));
	}
	return sendmsg(s, &m, MSG_NOSIGNAL) == 1 ? 0 : -1;
}
// Servidor del dueño: atiende peticiones "dame el descriptor del fileport N" (un uint32) de otros procesos.
static void* fp_server(void* arg) {
	int ls = (int)(long)arg;
	for (;;) {
		int s = accept4(ls, NULL, NULL, SOCK_CLOEXEC);
		if (s < 0) { if (errno == EINTR) continue; break; }
		uint32_t name;
		if (recv(s, &name, sizeof name, 0) == (ssize_t)sizeof name) send_fd(s, fp_local(name));
		close(s);
	}
	return NULL;
}
static void fp_server_start(void) {
	int ls = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
	struct sockaddr_un a; socklen_t al;
	fp_sockname(&a, &al, (int)getpid());
	if (ls < 0 || bind(ls, (struct sockaddr*)&a, al) != 0 || listen(ls, 16) != 0) {
		logf_("    fileport: no puedo abrir el servidor de <%d>: %s\n", (int)getpid(), strerror(errno));
		if (ls >= 0) close(ls);
		return;
	}
	pthread_attr_t at; pthread_attr_init(&at);
	pthread_attr_setdetachstate(&at, PTHREAD_CREATE_DETACHED);
	pthread_attr_setstacksize(&at, 1 << 21);   // hilos del anfitrión: el TLS estático del emulador vive en la pila
	pthread_t th;
	if (pthread_create(&th, &at, fp_server, (void*)(long)ls) != 0) close(ls);
}
// Tras fork, el hijo no tiene el hilo servidor (ni es dueño de los fileports del padre): se rearma al hacer uno propio.
void fileport_forked_child(void) { static const pthread_once_t init = PTHREAD_ONCE_INIT; fp_srv_once = init; memset(fp_tab, 0, sizeof fp_tab); fp_next = 0; }

static long bsd_fileport_makeport(struct ctx* c) {
	int fd = (int)c->a[0];
	int dup = fcntl(fd, F_DUPFD_CLOEXEC, FP_FD_MIN);
	if (dup < 0) return -darwin_errno(errno);
	pthread_once(&fp_srv_once, fp_server_start);
	uint32_t name = kobj_new(KO_FILEPORT, (int32_t)getpid());
	if (!name) { close(dup); return -35; /* EAGAIN */ }
	pthread_mutex_lock(&fp_lock);
	int slot = -1;
	for (int i = 0; i < FP_MAX && slot < 0; i++) if (!fp_tab[i].name) slot = i;
	if (slot < 0) { slot = fp_next; fp_next = (fp_next + 1) % FP_MAX; close(fp_tab[slot].fd); kobj_free(fp_tab[slot].name); }
	fp_tab[slot].name = name; fp_tab[slot].fd = dup;
	pthread_mutex_unlock(&fp_lock);
	if (trace_all || mach_trace()) logf_("    fileport_makeport <%d>: fd %d -> 0x%x\n", (int)getpid(), fd, name);
	if (safe_write(c->a[1], &name, 4) != 4) return -D_EFAULT;
	return 0;
}
static long bsd_fileport_makefd(struct ctx* c) {
	uint32_t name = (uint32_t)c->a[0];
	int type; int32_t owner;
	if (!kobj_lookup(name, &type, &owner) || type != KO_FILEPORT) return -D_EINVAL;
	int fd = -1;
	if (owner == (int32_t)getpid()) {
		int l = fp_local(name);
		if (l >= 0) fd = fcntl(l, F_DUPFD_CLOEXEC, 0);
	} else {
		int s = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
		struct sockaddr_un a; socklen_t al;
		fp_sockname(&a, &al, owner);
		if (s >= 0 && connect(s, (struct sockaddr*)&a, al) == 0 && send(s, &name, sizeof name, MSG_NOSIGNAL) == (ssize_t)sizeof name) {
			char ok = 0;
			struct iovec io = { &ok, 1 };
			union { struct cmsghdr h; char b[CMSG_SPACE(sizeof(int))]; } cm;
			struct msghdr m = { .msg_iov = &io, .msg_iovlen = 1, .msg_control = cm.b, .msg_controllen = sizeof cm.b };
			if (recvmsg(s, &m, MSG_CMSG_CLOEXEC) == 1 && ok) {
				struct cmsghdr* h = CMSG_FIRSTHDR(&m);
				if (h && h->cmsg_type == SCM_RIGHTS) memcpy(&fd, CMSG_DATA(h), sizeof(int));
			}
		}
		if (s >= 0) close(s);
	}
	if (trace_all || mach_trace()) logf_("    fileport_makefd <%d>: 0x%x (dueño %d) -> %d\n", (int)getpid(), name, (int)owner, fd);
	return fd >= 0 ? fd : -D_EINVAL;   // el dueño ya no lo tiene (o murió): como un puerto que no es un fileport
}

// ---- fork: candados de este módulo
// Se cogen antes del fork y se sueltan después (en el padre y en el hijo), para que el hijo no herede ninguno cogido
// (ver tahoe.h). El orden es el global: si el código normal puede tener uno cogido y pedir otro, el primero tiene que ir
// antes. fp_lock es un mutex normal (PTHREAD_MUTEX_INITIALIZER, ni recursive ni robust).
void emu_fileport_fork_lock(void) {
	pthread_mutex_lock(&fp_lock);
}
void emu_fileport_fork_unlock(void) {
	pthread_mutex_unlock(&fp_lock);
}

void emu_fileport_init(void) {
	reg_bsd(430, bsd_fileport_makeport);
	reg_bsd(431, bsd_fileport_makefd);
}
