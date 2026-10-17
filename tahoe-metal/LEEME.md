# tahoe-metal: el driver Metal de tahoe-run

Metal 2 sobre Vulkan, todo en Rust. Dos piezas:

- **tahoe-metal** (esta carpeta): el driver que corre *dentro* de macOS. Es el ejecutable de `TahoeMetal.bundle`, un
  Mach-O para x86_64 compilado aquí sin Xcode ni el SDK de Apple (ver `tools/macos-rs/LEEME.md`). Implementa las
  clases de Metal (`TahoeMTLDevice`, colas, buffers, bibliotecas, pipelines, command buffers y encoders) con el runtime
  de Objective-C.
- **tahoe-gpu servir** (`tahoe-gpu/src/servir.rs`): el servidor de GPU de Linux, el único proceso que usa Vulkan. El
  driver de cada proceso de macOS se conecta a él por un socket Unix (`tahoe-gpu/src/protocolo.rs`); los buffers son
  memoria compartida en `/dev/shm` que el servidor importa sin copias, y las bibliotecas Metal (AIR) se traducen allí a
  SPIR-V (`tahoe-gpu/src/air`).

## Cómo lo encuentra macOS

Con `TAHOE_METAL=<carpeta con TahoeMetal.bundle>`, tahoe-run:

1. publica un IOAccelerator (`TahoeAccelerator`) cuyo `MetalPluginName` es `TahoeMetal` y cuya propiedad
   `TahoeGPUSocket` es el socket del servidor (`TAHOE_GPU_SOCK`);
2. hace que `/System/Library/Extensions/TahoeMetal.bundle` lleve a esa carpeta (en la traducción de rutas: la copia de
   macOS no se toca).

Metal enumera los IOAccelerator, carga el bundle y crea su clase principal (`TahoeMTLDevice`, hija de `_MTLDevice`) con
`initWithAcceleratorPort:`. Sin `TAHOE_METAL` no hay acelerador y WindowServer compone por software, como siempre.

## Probar

```sh
tahoe-metal/empaquetar.sh                      # bundle y metalprobe en tahoe-metal/salida
(cd tahoe-gpu && cargo build --release)        # el servidor
cp tahoe-metal/salida/metalprobe "$TAHOE_ROOT/private/var/tmp/"
TAHOE_METAL=tahoe-metal/salida tools/macos-shell.sh --launchd -c '/private/var/tmp/metalprobe computo'
```

`metalprobe` lista los dispositivos (`MTLCopyAllDevices`) y, con `computo`, ejecuta un kernel de un metallib del propio
macOS (`SetValueFloat`) y una copia con un blit, y comprueba el resultado. El driver apunta en
`/private/var/tmp/tahoe-metal.log` (dentro de macOS) lo que Metal le pide y aún no tiene.
