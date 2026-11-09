//! `tahoe-gpu informe`: qué pide Metal 2 (familia Mac2, la de los Mac con Intel) y cómo se cubre en esta GPU.
//! Cada punto sale como «nativo» (Vulkan lo da tal cual), «emulado» (lo resuelve tahoe-gpu, normalmente al traducir
//! los shaders de Metal a SPIR-V) o «falta» (no hay forma razonable en esta GPU).

use ash::vk;

use crate::vk::Gpu;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Estado {
    Nativo,
    Emulado,
    Falta,
}

pub struct Punto {
    pub que: &'static str,
    pub estado: Estado,
    pub nota: String,
}

/// Lo que se mira de la GPU (separado de Vulkan para poder probar la evaluación sin GPU).
#[derive(Default, Clone)]
pub struct Capacidades {
    pub api: (u32, u32),
    pub image_cube_array: bool,
    pub bc: bool,
    pub d32s8: bool,
    pub msaa4: bool,
    pub anisotropy: bool,
    pub independent_blend: bool,
    pub dual_src_blend: bool,
    pub depth_clamp: bool,
    pub fill_lines: bool,
    pub layer_from_vertex: bool,
    pub color_attachments: u32,
    pub tessellation: bool,
    pub multi_draw_indirect: bool,
    pub first_instance: bool,
    pub max_tex_2d: u32,
    pub shared_memory: u32,
    pub max_invocations: u32,
    pub subgroup_ops: vk::SubgroupFeatureFlags,
    pub subgroup_stages: vk::ShaderStageFlags,
    pub float16: bool,
    pub int64: bool,
    pub int16: bool,
    pub variable_pointers: bool,
    pub buffer_device_address: bool,
    pub descriptor_indexing: bool,
    pub vertex_stores: bool,
    pub storage_read_without_format: bool,
    pub shader_interlock: bool,
    pub sample_locations: bool,
    pub timeline: bool,
    pub host_memory: bool,
    pub unified_memory: bool,
    pub max_storage_buffer: u32,
}

pub fn capacidades(g: &Gpu) -> Capacidades {
    let i = &g.instance;
    let mut f11 = vk::PhysicalDeviceVulkan11Features::default();
    let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
    let mut f2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut f11).push_next(&mut f12);
    let mut sg = vk::PhysicalDeviceSubgroupProperties::default();
    let mut p2 = vk::PhysicalDeviceProperties2::default().push_next(&mut sg);
    // SAFETY: dispositivo físico de esta instancia; cadenas pNext válidas mientras dura la llamada.
    unsafe {
        i.get_physical_device_features2(g.phys, &mut f2);
        i.get_physical_device_properties2(g.phys, &mut p2);
    }
    let f = f2.features;
    let l = &g.props.limits;
    // SAFETY: consulta de un formato en este dispositivo físico.
    let d32s8 = unsafe { i.get_physical_device_format_properties(g.phys, vk::Format::D32_SFLOAT_S8_UINT) }
        .optimal_tiling_features
        .contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT);
    let unified_memory = (0..g.mem_props.memory_type_count).any(|k| {
        g.mem_props.memory_types[k as usize]
            .property_flags
            .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL | vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT)
    });
    let v = g.props.api_version;
    Capacidades {
        api: (vk::api_version_major(v), vk::api_version_minor(v)),
        image_cube_array: f.image_cube_array != 0,
        bc: f.texture_compression_bc != 0,
        d32s8,
        msaa4: l.framebuffer_color_sample_counts.contains(vk::SampleCountFlags::TYPE_4),
        anisotropy: f.sampler_anisotropy != 0,
        independent_blend: f.independent_blend != 0,
        dual_src_blend: f.dual_src_blend != 0,
        depth_clamp: f.depth_clamp != 0,
        fill_lines: f.fill_mode_non_solid != 0,
        layer_from_vertex: f12.shader_output_layer != 0 && f12.shader_output_viewport_index != 0,
        color_attachments: l.max_color_attachments,
        tessellation: f.tessellation_shader != 0,
        multi_draw_indirect: f.multi_draw_indirect != 0,
        first_instance: f.draw_indirect_first_instance != 0,
        max_tex_2d: l.max_image_dimension2_d,
        shared_memory: l.max_compute_shared_memory_size,
        max_invocations: l.max_compute_work_group_invocations,
        subgroup_ops: sg.supported_operations,
        subgroup_stages: sg.supported_stages,
        float16: f12.shader_float16 != 0,
        int64: f.shader_int64 != 0,
        int16: f.shader_int16 != 0,
        variable_pointers: f11.variable_pointers != 0,
        buffer_device_address: f12.buffer_device_address != 0,
        descriptor_indexing: f12.descriptor_indexing != 0 && f12.runtime_descriptor_array != 0,
        vertex_stores: f.vertex_pipeline_stores_and_atomics != 0,
        storage_read_without_format: f.shader_storage_image_read_without_format != 0,
        shader_interlock: g.has_ext("VK_EXT_fragment_shader_interlock"),
        sample_locations: g.has_ext("VK_EXT_sample_locations"),
        timeline: f12.timeline_semaphore != 0,
        host_memory: g.host_memory.is_some(),
        unified_memory,
        max_storage_buffer: l.max_storage_buffer_range,
    }
}

fn si(b: bool, nativo: Estado) -> Estado {
    if b { Estado::Nativo } else { nativo }
}

/// Requisitos de Metal 2 (MTLGPUFamilyMac2) frente a las capacidades.
pub fn evaluar(c: &Capacidades) -> Vec<Punto> {
    use Estado::*;
    let p = |que, estado, nota: &str| Punto { que, estado, nota: nota.to_string() };
    let sg_need = vk::SubgroupFeatureFlags::BASIC | vk::SubgroupFeatureFlags::VOTE | vk::SubgroupFeatureFlags::BALLOT
        | vk::SubgroupFeatureFlags::SHUFFLE | vk::SubgroupFeatureFlags::ARITHMETIC;
    let sg_ok = c.subgroup_ops.contains(sg_need)
        && c.subgroup_stages.contains(vk::ShaderStageFlags::COMPUTE | vk::ShaderStageFlags::FRAGMENT);
    vec![
        p("Vulkan 1.1 o más", si(c.api >= (1, 1), Falta), &format!("la GPU da {}.{}", c.api.0, c.api.1)),
        p("Memoria compartida CPU/GPU (MTLStorageModeShared)", si(c.unified_memory && c.host_memory, if c.unified_memory { Emulado } else { Falta }),
          "GPU integrada: memoria del anfitrión importada (VK_EXT_external_memory_host), sin copias"),
        p("Texturas cube array", si(c.image_cube_array, Falta), ""),
        p("Compresión BC (DXT)", si(c.bc, Emulado), "sin ella, se descomprimiría al subir la textura"),
        p("Depth32Float_Stencil8", si(c.d32s8, Emulado), "sin él, profundidad y esténcil por separado"),
        p("MSAA 4x", si(c.msaa4, Falta), ""),
        p("Filtrado anisótropo", si(c.anisotropy, Emulado), ""),
        p("Mezcla independiente y de dos fuentes", si(c.independent_blend && c.dual_src_blend, Falta), ""),
        p("Depth clamp y relleno en líneas", si(c.depth_clamp && c.fill_lines, Emulado), ""),
        p("Capa y viewport desde el vertex shader", si(c.layer_from_vertex, Falta), "render_target_array_index / viewport_array_index"),
        p("8 attachments de color", si(c.color_attachments >= 8, Falta), &format!("{}", c.color_attachments)),
        p("Teselado", si(c.tessellation, Emulado), "Metal lo hace con un kernel de cómputo y un post-tessellation vertex shader"),
        p("Dibujo indirecto múltiple", si(c.multi_draw_indirect && c.first_instance, Emulado), ""),
        p("Indirect command buffers", Emulado, "se reproducen como dibujos indirectos de Vulkan"),
        p("Texturas de 16384", si(c.max_tex_2d >= 16384, Falta), &format!("máximo {}", c.max_tex_2d)),
        p("Memoria de threadgroup de 32 KB", si(c.shared_memory >= 32768, Falta), &format!("{} bytes", c.shared_memory)),
        p("1024 hilos por threadgroup", si(c.max_invocations >= 1024, Falta), &format!("{}", c.max_invocations)),
        p("Funciones SIMD-group (Metal 2.1)", si(sg_ok, Emulado), "subgroups de Vulkan; sin ellos, por memoria de threadgroup"),
        p("half (float de 16 bits)", si(c.float16, Emulado), "se calcula en float de 32 bits (más precisión, mismo resultado visual)"),
        p("long/ulong (enteros de 64 bits)", si(c.int64, Emulado), "en pares de enteros de 32 bits"),
        p("short/char (16 y 8 bits)", si(c.int16, Emulado), "ensanchados a 32 bits; en buffers, por bytes con desplazamientos"),
        p("Punteros device/constant en shaders", si(c.buffer_device_address, if c.variable_pointers { Emulado } else { Falta }),
          "sin buffer_device_address: puntero = (buffer, desplazamiento), con variable pointers"),
        p("Argument buffers (tier 1)", Emulado, "como conjuntos de descriptores"),
        p("Argument buffers tier 2", si(c.descriptor_indexing && c.buffer_device_address, Falta), "los Mac con Intel solo tienen tier 1"),
        p("Escritura en buffers desde vertex shaders", si(c.vertex_stores, Falta), "poco usado (lo usan algunos juegos)"),
        p("Texturas read_write sin formato fijo", si(c.storage_read_without_format, Emulado), "una variante del shader por formato de textura"),
        p("Raster order groups", si(c.shader_interlock, Falta), "VK_EXT_fragment_shader_interlock"),
        p("Posiciones de muestra programables", si(c.sample_locations, Falta), ""),
        p("MTLSharedEvent / MTLFence", si(c.timeline, Emulado), "semáforos timeline"),
        p("Buffers grandes", si(c.max_storage_buffer >= 1 << 30, Emulado), &format!("máximo {} MB por binding; los mayores se trocean", c.max_storage_buffer >> 20)),
    ]
}

pub fn informe(g: &Gpu) {
    let c = capacidades(g);
    println!("GPU: {}  (Vulkan {}.{}, {} extensiones)", g.name(), c.api.0, c.api.1, g.extensions.len());
    println!();
    let pts = evaluar(&c);
    for pt in &pts {
        let e = match pt.estado {
            Estado::Nativo => "nativo ",
            Estado::Emulado => "emulado",
            Estado::Falta => "FALTA  ",
        };
        if pt.nota.is_empty() { println!("  [{e}] {}", pt.que) } else { println!("  [{e}] {} — {}", pt.que, pt.nota) }
    }
    let n = |e| pts.iter().filter(|p| p.estado == e).count();
    println!();
    println!("Metal 2: {} nativos, {} emulados, {} faltan", n(Estado::Nativo), n(Estado::Emulado), n(Estado::Falta));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_nada_falta_lo_esencial() {
        let pts = evaluar(&Capacidades::default());
        let falta = |q: &str| pts.iter().find(|p| p.que.starts_with(q)).map(|p| p.estado);
        assert_eq!(falta("Vulkan 1.1"), Some(Estado::Falta));
        assert_eq!(falta("half"), Some(Estado::Emulado));
        assert_eq!(falta("Indirect command buffers"), Some(Estado::Emulado));
    }

    #[test]
    fn punteros_con_variable_pointers_se_emulan() {
        let c = Capacidades { variable_pointers: true, ..Default::default() };
        let pts = evaluar(&c);
        let p = pts.iter().find(|p| p.que.starts_with("Punteros")).unwrap();
        assert_eq!(p.estado, Estado::Emulado);
    }
}
