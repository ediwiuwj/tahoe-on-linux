# Herramientas históricas de Darling (no usar)

Estas tres herramientas son de la primera etapa del proyecto, de antes de que existiera `tahoe-run`, cuando se
probó si [Darling](https://github.com/darlinghq/darling) servía para arrancar macOS 26. La conclusión fue hacer una
capa propia: **`tahoe-run` no usa código de Darling** ni necesita estas herramientas para compilarse, arrancar macOS ni
probarse. Se guardan solo como registro de cómo se llegó a esa decisión.

| Herramienta | Qué hacía |
|---|---|
| `build_darling_codespace.sh` | En un GitHub Codespace, clonaba Darling con sus submódulos y compilaba dos piezas suyas: `mldr` (su cargador de programas Mach-O) y `darlingserver` (su servidor que imita al núcleo). No compila nada de este proyecto. |
| `dyld_iterate.py` | Ejecutaba un programa de macOS 26 dentro de Darling y, cada vez que faltaba una biblioteca, la sacaba de la `dyld_shared_cache` y reintentaba, para ver hasta dónde llegaba Darling. |
| `compare_syscalls.py` | Contaba cuántas llamadas al sistema BSD y trampas Mach del XNU de Apple implementa Darling. |

Las compilaciones de `tahoe-run` son ligeras (`tahoe-run/build.sh`, unos segundos) y se hacen en la propia PC.
