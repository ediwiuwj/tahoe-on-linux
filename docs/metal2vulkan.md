# Integración Metal2Vulkan: estado experimental

Revisión fijada: `219f01365dc4c8eb346816c405b5b17403793764` de
https://github.com/steelbrain/metal2vulkan. Requiere Rust 1.98 o posterior.

## Implementado en este primer corte

La característica Cargo opcional `metal2vulkan` añade el traductor upstream
al servidor Linux, no al bundle Mach-O. El comando:

```sh
tahoe-gpu metal2vulkan archivo.metallib
```

lee la biblioteca mediante `metal2vulkan::library::Library` y enumera sus
funciones y etapas. También acepta AIR bitcode admitido por upstream. Sin la
característica, el comando informa que el soporte no está habilitado.

Esto **no cambia todavía la traducción ni la ejecución de shaders del servidor**.
`servir` conserva el backend Tahoe. La siguiente etapa debe adaptar los planes
de pipeline y bindings upstream al motor, incluyendo especialización, tamaños
de grupos y disposición de recursos. No es correcto sustituir solo el SPIR-V.
El protocolo, IOKit y las copias IOSurface existentes no se modifican.

## Validación remota

El workflow `Integración Metal2Vulkan` comprueba ambas configuraciones y dos
casos de entrada inválida. No requiere GPU ni archivos Apple. No demuestra
compatibilidad con Recovery, ejecución de cómputo, render ni presentación.
Debe añadirse un fixture válido propio y una prueba de ejecución Vulkan antes
de considerar funcional el backend. No se han ejecutado compilaciones locales.

La dependencia nueva requiere actualizar `tahoe-gpu/Cargo.lock`; el workflow
resuelve las dependencias y conserva el lockfile como artefacto para revisarlo
e incorporarlo. Hasta entonces, esta validación no utiliza `--locked`.

## Licencias y distribución

El código propio sigue bajo su licencia existente. Upstream ofrece
`GPL-2.0-or-later OR LGPL-3.0-or-later`, no MIT. Antes de distribuir un binario
que lo enlace deben revisarse la opción de licencia, los avisos, las fuentes
correspondientes y los requisitos de reenlace aplicables. Este primer corte
no prepara ni publica artefactos de distribución.

IOSurface, Mach VM, CAMetalLayer y presentación siguen siendo responsabilidades
de Tahoe; añadir esta biblioteca no implementa esos subsistemas.
