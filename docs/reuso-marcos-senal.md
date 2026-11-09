# Reutilización de zonas de marcos de señal

Estado: implementación pendiente de CI y pruebas reales.

El registro privado tiene ocho posiciones. La prueba invitada confirmó que
siglongjmp fuera del handler dejaba posiciones activas: el noveno envío fallaba.
No se vacía el registro por UC_RESET_ALT_STACK ni se interpreta cualquier RSP
mayor como prueba de abandono.

La nueva decisión está en `nucleo/src/marcos_activos.rs` (Rust seguro): si una
nueva entrega va a escribir sobre la zona de un marco anterior, ese marco y
sus descendientes ya no pueden conservar un sigreturn válido. Se retiran sus
tokens al publicar la nueva entrega. Las zonas disjuntas no se retiran.

La preparación utiliza scratch separado para no modificar registros privados
antes de validar. Las escrituras guest NO son atómicas: si fallan parcialmente
sobre un marco anterior, el emulador termina ese proceso de forma controlada,
en lugar de conservar tokens para un contexto potencialmente corrompido.
Si no hay solapamiento con registros activos, devuelve EFAULT.

Un sigreturn con dirección y token válidos de un marco anterior puede retornar
al exterior y retirar descendientes abandonados. Primero valida todo el estado;
no restaura registros parcialmente ni libera otros tokens ante token inválido.

La prueba `signaljumpprobe` exige doce saltos fuera de un handler y otros doce
saltos desde un handler interno al externo, que después retorna normalmente.
También deben seguir pasando los handlers anidados sin salto y las máscaras.

Limitaciones: no se recolectan contextos abandonados en zonas disjuntas sin
retorno explícito; ocho contextos simultáneos siguen siendo el límite. No es
soporte universal de fibras/cambios de pila, ni prueba por sí solo de Ctrl-C.
