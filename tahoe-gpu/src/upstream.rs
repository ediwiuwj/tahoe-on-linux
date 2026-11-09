//! Frontera de integración con la revisión fijada de Metal2Vulkan.
//! Todavía no adapta sus planes de bindings al motor Vulkan de Tahoe.

use metal2vulkan::library::Library;

pub fn listar(ruta: Option<String>) -> crate::vk::Res<()> {
    let ruta = ruta.ok_or("uso: tahoe-gpu metal2vulkan <archivo.metallib|archivo.bc>")?;
    let datos = std::fs::read(&ruta).map_err(|e| format!("{ruta}: {e}"))?;
    let biblioteca = Library::parse(&datos)
        .map_err(|e| format!("Metal2Vulkan: {e}"))?;
    for funcion in biblioteca.functions() {
        println!("{:?}\t{}", funcion.stage, funcion.name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rechaza_entrada_vacia() {
        assert!(Library::parse(&[]).is_err());
    }

    #[test]
    fn rechaza_entrada_que_no_es_metal() {
        assert!(Library::parse(b"Esto no es una biblioteca Metal").is_err());
    }
}
