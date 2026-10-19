# Rust para el macOS de tahoe-run

Programas y bibliotecas en Rust (con `std`) que se ejecutan *dentro* del macOS de tahoe-run, sin Xcode ni el SDK de
Apple: el driver Metal (`tahoe-metal`) y herramientas de prueba.

1. `rustup target add x86_64-apple-darwin` (la biblioteca estándar de Rust para macOS).
2. Descripciones de las bibliotecas de macOS, sacadas de la caché de dyld de tu copia (no se sube nada de Apple):
   `python3 tools/macos-rs/gen_tbd.py ~/.cache/tahoe-syms/<caché> ~/.cache/tahoe-tbd`
3. En el proyecto, `.cargo/config.toml`:
   ```toml
   [target.x86_64-apple-darwin]
   linker = "rust-lld"
   rustflags = ["-C", "linker-flavor=ld64.lld", "-C", "link-arg=-L/ruta/a/.cache/tahoe-tbd",
                "-C", "link-arg=-platform_version", "-C", "link-arg=macos", "-C", "link-arg=11.0", "-C", "link-arg=26.0",
                "-C", "link-arg=-arch", "-C", "link-arg=x86_64"]
   ```
4. `cargo build --release --target x86_64-apple-darwin`, copiar el binario dentro de macOS y ejecutarlo, p. ej.
   `tools/macos-shell.sh -c /private/var/tmp/programa`.

Probado con un programa con `Vec`, `println!` y `thread::spawn`/`join` (que destapó un fallo de `bsdthread_create`).
