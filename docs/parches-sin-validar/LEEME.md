# Parches sin validar

Cambios a medias que no se subieron al código porque no pasaron (o no llegaron a pasar) la batería de pruebas. Se guardan
aquí para no perderlos. Para probar uno sobre la rama actual:

```sh
git apply --check docs/parches-sin-validar/<parche>.patch   # ¿aplica limpio?
git apply docs/parches-sin-validar/<parche>.patch           # aplicarlo (git checkout -- <archivo> para deshacerlo)
```

| Parche | Qué hace | Por qué no se subió |
|---|---|---|
| `contenedor-apfs.patch` | Cliente de AppleAPFSContainer (`emu_iokit.inc`): selector 8 (APFSExtendedSpaceInfo: «size», «used», «reserve», «Max volume count»), 9 (rol de cada volumen), 0x12 y 0x1e (a cero). Para las capacidades de `diskutil apfs list` (hoy «ERROR -69808»). | storagekitd lanza una excepción justo después del selector 8 y se cae (SIGSEGV en `_Unwind_RaiseException`); con él, `diskutil list` sale vacío. Siguiente paso: ver el motivo con puntos de ruptura en `objc_exception_throw` (0x7ff802a35ff4) y `__cxa_throw` (0x7ff802e33d77) en storagekitd (`/dev/shm/tahoe-break`, PEEK `*rdi+10`). También pide el selector 55 (entra 4 bytes, salen 8). |
| `log-con-hora.patch` | Con `/dev/shm/tahoe-log-time`, cada línea del registro lleva delante el reloj monótono (para medir esperas). Lo usan `tools/diag/arranque.sh`, `dk_calmo.sh`, `llaveros.sh`. | Solo diagnóstico; no hace falta para funcionar. Aplicarlo cuando se midan tiempos. |
| `puertos-envio-16384.patch` | Tabla de puertos Mach y de derechos de envío de 8192 a 16384 (el macOS completo usa unos 6000) y búsqueda de hueco desde una pista. | Pendiente de prueba (A/B y batería). |
