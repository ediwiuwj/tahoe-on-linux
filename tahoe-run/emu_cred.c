// Credenciales de proceso emuladas (uid/gid real, efectivo y guardado, y grupos), con las reglas de setuid(2) de XNU.
//
// Todo tahoe-run corre como root dentro de un espacio de nombres de usuario que solo traduce el uid 0, así que Linux no
// puede cambiar de identidad de verdad. Pero los daemons de macOS comprueban quiénes son: launchd/xpcproxy les ponen el
// UserName/GroupName de su plist (setgid, initgroups, setuid) y luego, p. ej., displaypolicyd aborta si getegid() no es el
// de _displaypolicyd. Las credenciales viven en el proceso; fork las copia y exec/posix_spawn las pasan por el entorno
// (TAHOE_CRED). El acceso a archivos sigue siendo el del anfitrión (root), como un sistema sin comprobación de permisos.
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "tahoe.h"

#define NGROUPS_MAX_ 16
static uint32_t ruid, euid, svuid, rgid, egid, svgid;
static uint32_t groups[NGROUPS_MAX_];
static int ngroups = 1;                       // como root: grupo 0

uint32_t cred_euid(void) { return euid; }
uint32_t cred_egid(void) { return egid; }

// TAHOE_CRED=ruid,euid,svuid,rgid,egid,svgid,g1,g2...
void cred_env(char* out, size_t cap) {
	int k = snprintf(out, cap, "TAHOE_CRED=%u,%u,%u,%u,%u,%u", ruid, euid, svuid, rgid, egid, svgid);
	for (int i = 0; i < ngroups && k > 0 && (size_t)k < cap; i++) k += snprintf(out + k, cap - (size_t)k, ",%u", groups[i]);
}
static void cred_load(void) {
	const char* v = getenv("TAHOE_CRED");
	if (!v) return;
	unsigned long x[6 + NGROUPS_MAX_]; int n = 0;
	char* end;
	while (*v && n < 6 + NGROUPS_MAX_) { x[n++] = strtoul(v, &end, 10); if (*end != ',') break; v = end + 1; }
	if (n < 6) return;
	ruid = (uint32_t)x[0]; euid = (uint32_t)x[1]; svuid = (uint32_t)x[2]; rgid = (uint32_t)x[3]; egid = (uint32_t)x[4]; svgid = (uint32_t)x[5];
	ngroups = 0;
	for (int i = 6; i < n; i++) groups[ngroups++] = (uint32_t)x[i];
}

#define D_EPERM 1
static long b_getuid(struct ctx* c)  { (void)c; return ruid; }
static long b_geteuid(struct ctx* c) { (void)c; return euid; }
static long b_getgid(struct ctx* c)  { (void)c; return rgid; }
static long b_getegid(struct ctx* c) { (void)c; return egid; }
// setuid: el superusuario cambia los tres; si no, solo el efectivo y solo al real o al guardado.
static long b_setuid(struct ctx* c) {
	uint32_t u = (uint32_t)c->a[0];
	if (euid == 0) { ruid = euid = svuid = u; return 0; }
	if (u == ruid || u == svuid) { euid = u; return 0; }
	return -D_EPERM;
}
static long b_seteuid(struct ctx* c) {
	uint32_t u = (uint32_t)c->a[0];
	if (euid != 0 && u != ruid && u != svuid) return -D_EPERM;
	euid = u; return 0;
}
static long b_setgid(struct ctx* c) {
	uint32_t g = (uint32_t)c->a[0];
	if (euid == 0) { rgid = egid = svgid = g; return 0; }
	if (g == rgid || g == svgid) { egid = g; return 0; }
	return -D_EPERM;
}
static long b_setegid(struct ctx* c) {
	uint32_t g = (uint32_t)c->a[0];
	if (euid != 0 && g != rgid && g != svgid) return -D_EPERM;
	egid = g; return 0;
}
// setreuid/setregid(real, efectivo): -1 deja ese valor como estaba.
static long b_setreuid(struct ctx* c) {
	uint32_t r = (uint32_t)c->a[0], e = (uint32_t)c->a[1];
	if (euid != 0 && ((r != (uint32_t)-1 && r != ruid && r != euid) || (e != (uint32_t)-1 && e != ruid && e != euid && e != svuid))) return -D_EPERM;
	if (r != (uint32_t)-1) ruid = r;
	if (e != (uint32_t)-1) euid = e;
	if (r != (uint32_t)-1 || (e != (uint32_t)-1 && e != ruid)) svuid = euid;
	return 0;
}
static long b_setregid(struct ctx* c) {
	uint32_t r = (uint32_t)c->a[0], e = (uint32_t)c->a[1];
	if (euid != 0 && ((r != (uint32_t)-1 && r != rgid && r != egid) || (e != (uint32_t)-1 && e != rgid && e != egid && e != svgid))) return -D_EPERM;
	if (r != (uint32_t)-1) rgid = r;
	if (e != (uint32_t)-1) egid = e;
	if (r != (uint32_t)-1 || (e != (uint32_t)-1 && e != rgid)) svgid = egid;
	return 0;
}
// getgroups(n, lista): con n = 0 solo cuántos.
static long b_getgroups(struct ctx* c) {
	uint32_t n = (uint32_t)c->a[0];
	if (n == 0) return ngroups;
	if (n < (uint32_t)ngroups) return -D_EINVAL;
	return safe_write(c->a[1], groups, (size_t)ngroups * 4) == (ssize_t)(ngroups * 4) ? ngroups : -D_EFAULT;
}
static long b_setgroups(struct ctx* c) {
	uint32_t n = (uint32_t)c->a[0];
	if (euid != 0) return -D_EPERM;
	if (n > NGROUPS_MAX_) return -D_EINVAL;
	if (n && safe_read(c->a[1], groups, (size_t)n * 4) != (ssize_t)(n * 4)) return -D_EFAULT;
	ngroups = (int)n;
	return 0;
}
// initgroups(n, lista, uid de gmuid): como setgroups (el núcleo guarda además el uid de pertenencia externa)
static long b_initgroups(struct ctx* c) { return b_setgroups(c); }

// identitysvc(opcode, mensaje): el resolutor de identidades externas del núcleo (kern_credential.c). opendirectoryd se
// registra como resolutor (KAUTH_EXTLOOKUP_REGISTER 0) y sus hilos trabajadores esperan en el núcleo peticiones de
// traducción uid/gid <-> GUID (KAUTH_EXTLOOKUP_WORKER 2, con RESULT 1 para devolver la anterior). Aquí el núcleo nunca
// pregunta (los permisos los decide Linux), así que el trabajador duerme hasta una señal, como en XNU sin peticiones.
// Las cachés de identidades (GET/SET_CACHE_SIZES 8/16, CLEAR_CACHES 32) son las de XNU por omisión, sin efecto.
// Sin esta llamada opendirectoryd reintentaba el registro sin parar (CPU al 100%).
#include <unistd.h>
static long b_identitysvc(struct ctx* c) {
	uint32_t op = (uint32_t)c->a[0];
	if (op == 0) return euid == 0 ? 0 : -D_EPERM;                          // REGISTER: solo root
	if (op & 4) return 0;                                                   // DEREGISTER
	if (op & 8) {                                                           // GET_CACHE_SIZES: struct kauth_cache_sizes
		uint32_t sz[2] = { 100, 100 };                                      // kcs_group_size, kcs_id_size (XNU por omisión)
		return safe_write(c->a[1], sz, sizeof sz) == (ssize_t)sizeof sz ? 0 : -D_EFAULT;
	}
	if (op & (16 | 32)) return euid == 0 ? 0 : -D_EPERM;                     // SET_CACHE_SIZES / CLEAR_CACHES
	if (op & 2) { pause(); return -4; }                                     // WORKER: sin peticiones; EINTR al llegar una señal
	if (op & 1) return 0;                                                   // RESULT suelto: nada pendiente
	return -D_EINVAL;
}

void emu_cred_init(void) {
	cred_load();
	reg_bsd(24, b_getuid); reg_bsd(25, b_geteuid); reg_bsd(47, b_getgid); reg_bsd(43, b_getegid);
	reg_bsd(23, b_setuid); reg_bsd(183, b_seteuid); reg_bsd(181, b_setgid); reg_bsd(182, b_setegid);
	reg_bsd(126, b_setreuid); reg_bsd(127, b_setregid);
	reg_bsd(79, b_getgroups); reg_bsd(80, b_setgroups); reg_bsd(243, b_initgroups);
	reg_bsd(293, b_identitysvc);
}
