# Parches sin validar

Cambios a medias que no se subieron al código porque no pasaron (o no llegaron a pasar) la batería de pruebas. Se guardan
aquí para no perderlos. Para probar uno sobre la rama actual:

```sh
git apply --check docs/parches-sin-validar/<parche>.patch   # ¿aplica limpio?
git apply docs/parches-sin-validar/<parche>.patch           # aplicarlo (git checkout -- <archivo> para deshacerlo)
```

| Parche | Qué hace | Por qué no se subió |
|---|---|---|
| `log-con-hora.patch` | Con `/dev/shm/tahoe-log-time`, cada línea del registro lleva delante el reloj monótono (para medir esperas). Lo usan `tools/diag/arranque.sh`, `dk_calmo.sh`, `llaveros.sh`. | Solo diagnóstico; no hace falta para funcionar. Aplicarlo cuando se midan tiempos. |
| `puertos-envio-16384.patch` | Tabla de puertos Mach y de derechos de envío de 8192 a 16384 (el macOS completo usa unos 6000) y búsqueda de hueco desde una pista. | Pendiente de prueba (A/B y batería). |
