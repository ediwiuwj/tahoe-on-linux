//! tahoe-gpu: la GPU del PC para el macOS que corre en tahoe-run (Metal sobre Vulkan, lado Linux).
//!
//! Por ahora, órdenes para conocer y probar la GPU y los shaders de Metal:
//! - `tahoe-gpu informe`: el dispositivo Vulkan y, punto por punto, qué pide Metal 2 y cómo se cubre en esta GPU
//!   (de forma nativa, emulado en el traductor de shaders o pendiente).
//! - `tahoe-gpu prueba`: un shader de cómputo (SPIR-V generado aquí con rspirv) sobre memoria compartida: un memfd
//!   proyectado e importado en Vulkan con VK_EXT_external_memory_host, que es como llegarán los MTLBuffer de macOS
//!   (la misma memoria la ven el programa de macOS y la GPU, sin copias).
//! - `tahoe-gpu metallib <archivo>`: las funciones de una biblioteca de shaders de Metal (tipo, versión AIR, bitcode).

mod informe;
mod metallib;
mod prueba;
mod shaders;
mod vk;

fn main() {
    let orden = std::env::args().nth(1).unwrap_or_default();
    let r = match orden.as_str() {
        "informe" => vk::Gpu::new().map(|g| informe::informe(&g)),
        "prueba" => vk::Gpu::new().and_then(|g| prueba::prueba(&g)),
        "metallib" => listar_metallib(std::env::args().nth(2)),
        _ => {
            eprintln!("uso: tahoe-gpu informe | prueba | metallib <archivo>");
            std::process::exit(2);
        }
    };
    if let Err(e) = r {
        eprintln!("tahoe-gpu: {e}");
        std::process::exit(1);
    }
}

fn listar_metallib(ruta: Option<String>) -> vk::Res<()> {
    let ruta = ruta.ok_or("falta el archivo .metallib")?;
    let d = std::fs::read(&ruta).map_err(|e| format!("{ruta}: {e}"))?;
    let b = metallib::Biblioteca::leer(&d)?;
    println!("{ruta}: metallib {}.{}, tipo {}, {} funciones", b.version.0, b.version.1, b.tipo_archivo, b.funciones.len());
    for f in &b.funciones {
        let bc = b.bitcode(f)?;
        let magia = bc.get(0..4).map(|m| u32::from_le_bytes(m.try_into().unwrap())).unwrap_or(0);
        println!(
            "  {:<9} {:<50} AIR {:?} MSL {:?}  bitcode {} bytes ({})",
            format!("{:?}", f.tipo),
            f.nombre,
            f.air.unwrap_or_default(),
            f.lenguaje.unwrap_or_default(),
            bc.len(),
            if magia == 0x0b17c0de || magia == 0xdec04342 { "LLVM" } else { "¿?" }
        );
    }
    Ok(())
}
