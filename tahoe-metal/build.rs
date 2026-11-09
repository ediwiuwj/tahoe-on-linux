// Las bibliotecas de macOS (libSystem, libobjc) se enlazan con descripciones .tbd generadas de la caché de dyld de cada
// cual (tools/macos-rs/gen_tbd.py): TAHOE_TBD o ~/.cache/tahoe-tbd.
fn main() {
    let dir = std::env::var("TAHOE_TBD").unwrap_or_else(|_| format!("{}/.cache/tahoe-tbd", std::env::var("HOME").unwrap_or_default()));
    println!("cargo:rustc-link-search=native={dir}");
    println!("cargo:rerun-if-env-changed=TAHOE_TBD");
}
