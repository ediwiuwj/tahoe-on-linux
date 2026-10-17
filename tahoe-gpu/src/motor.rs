//! Motor de ejecución sobre Vulkan: lo que hará con la GPU el driver Metal de macOS. Buffers en memoria compartida
//! (memfd importado sin copias), texturas, samplers, pipelines a partir de shaders traducidos (`air::spirv`) y
//! despachos de cómputo. Las órdenes se ejecutan una a una con espera (aún sin colas asíncronas).

use std::collections::HashMap;

use ash::vk;

use crate::air::spirv::{Recurso, Traduccion};
use crate::vk::{err, Gpu, Res};

pub struct Motor {
    pub gpu: Gpu,
    pub(crate) pool: vk::CommandPool,
    fijo: std::sync::OnceLock<vk::Sampler>,
}

/// Buffer de Metal: memoria compartida (memfd) que ven a la vez la CPU, macOS y la GPU.
pub struct Buffer {
    pub ptr: *mut u8,
    pub len: usize,
    pub(crate) buf: vk::Buffer,
    mem: vk::DeviceMemory,
}

/// Forma de una textura (los tipos de MTLTextureType que hay en Vulkan).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Forma {
    D1,
    D1Array,
    D2,
    D2Array,
    D3,
    Cubo,
    CuboArray,
}

impl Forma {
    /// La forma de una textura de un shader traducido.
    pub fn de_spirv(dim: rspirv::spirv::Dim, arreglo: bool) -> Forma {
        use rspirv::spirv::Dim;
        match (dim, arreglo) {
            (Dim::Dim1D, false) => Forma::D1,
            (Dim::Dim1D, true) => Forma::D1Array,
            (Dim::Dim3D, _) => Forma::D3,
            (Dim::DimCube, false) => Forma::Cubo,
            (Dim::DimCube, true) => Forma::CuboArray,
            (_, true) => Forma::D2Array,
            _ => Forma::D2,
        }
    }
}

pub struct Textura {
    pub ancho: u32,
    pub alto: u32,
    /// profundidad (3D) o 1
    pub prof: u32,
    /// capas (arrays; 6 por cubo) o 1
    pub capas: u32,
    pub formato: vk::Format,
    pub bpp: u32,
    pub(crate) image: vk::Image,
    mem: vk::DeviceMemory,
    pub vista: vk::ImageView,
}

pub struct PipelineComputo {
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    set_layout: vk::DescriptorSetLayout,
    enlaces: Vec<(u32, vk::DescriptorType)>,
}

/// Lo que se enlaza en un despacho, por binding.
pub enum Enlazado<'a> {
    Buffer(&'a Buffer),
    /// buffer desde un desplazamiento (setBuffer:offset:atIndex:); alineado a minStorageBufferOffsetAlignment
    BufferDesde(&'a Buffer, u64),
    Textura(&'a Textura),
    Sampler(vk::Sampler),
}

fn tipo_descriptor(r: &Recurso) -> vk::DescriptorType {
    match r {
        Recurso::Buffer { .. } => vk::DescriptorType::STORAGE_BUFFER,
        Recurso::Textura { .. } => vk::DescriptorType::SAMPLED_IMAGE,
        Recurso::TexturaEscritura { .. } => vk::DescriptorType::STORAGE_IMAGE,
        Recurso::Sampler { .. } | Recurso::SamplerFijo { .. } => vk::DescriptorType::SAMPLER,
    }
}

impl Motor {
    pub fn new() -> Res<Motor> {
        let gpu = Gpu::new()?;
        let ci = vk::CommandPoolCreateInfo::default().queue_family_index(gpu.queue_family).flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        // SAFETY: dispositivo válido.
        let pool = unsafe { gpu.device.create_command_pool(&ci, None) }.map_err(err("vkCreateCommandPool"))?;
        Ok(Motor { gpu, pool, fijo: std::sync::OnceLock::new() })
    }

    /// Graba y ejecuta órdenes, esperando a que acaben.
    pub fn una_vez(&self, f: impl FnOnce(&ash::Device, vk::CommandBuffer)) -> Res<()> {
        let d = &self.gpu.device;
        // SAFETY: objetos de este dispositivo; se espera a la valla antes de liberar el command buffer.
        unsafe {
            let cb = d.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(self.pool).command_buffer_count(1)).map_err(err("cb"))?[0];
            d.begin_command_buffer(cb, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT)).map_err(err("begin"))?;
            f(d, cb);
            d.end_command_buffer(cb).map_err(err("end"))?;
            let fence = d.create_fence(&vk::FenceCreateInfo::default(), None).map_err(err("fence"))?;
            let cbs = [cb];
            d.queue_submit(self.gpu.queue, &[vk::SubmitInfo::default().command_buffers(&cbs)], fence).map_err(err("vkQueueSubmit"))?;
            let r = d.wait_for_fences(&[fence], true, 10_000_000_000).map_err(err("vkWaitForFences"));
            d.destroy_fence(fence, None);
            d.free_command_buffers(self.pool, &cbs);
            r
        }
    }

    /// Buffer sobre una memoria compartida ya proyectada (`ptr` alineado a página, `len` múltiplo de página).
    ///
    /// # Safety
    /// `ptr` debe apuntar a `len` bytes proyectados que vivan más que el Buffer.
    pub unsafe fn buffer_en(&self, ptr: *mut u8, len: usize) -> Res<Buffer> {
        let d = &self.gpu.device;
        let hm = self.gpu.host_memory.as_ref().ok_or("la GPU no puede importar memoria del anfitrión")?;
        // SAFETY: lo garantiza quien llama (ptr válido); objetos de este dispositivo.
        unsafe {
            let mut hp = vk::MemoryHostPointerPropertiesEXT::default();
            (hm.fp().get_memory_host_pointer_properties_ext)(d.handle(), vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT, ptr.cast(), &mut hp)
                .result()
                .map_err(err("vkGetMemoryHostPointerPropertiesEXT"))?;
            let mut ext = vk::ExternalMemoryBufferCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT);
            let usos = vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::VERTEX_BUFFER | vk::BufferUsageFlags::INDEX_BUFFER | vk::BufferUsageFlags::INDIRECT_BUFFER | vk::BufferUsageFlags::UNIFORM_BUFFER;
            let buf = d.create_buffer(&vk::BufferCreateInfo::default().size(len as u64).usage(usos).push_next(&mut ext), None).map_err(err("vkCreateBuffer"))?;
            let req = d.get_buffer_memory_requirements(buf);
            let ty = self.gpu.memory_type(req.memory_type_bits & hp.memory_type_bits, vk::MemoryPropertyFlags::HOST_VISIBLE).ok_or("sin tipo de memoria para importar")?;
            let mut imp = vk::ImportMemoryHostPointerInfoEXT::default().handle_type(vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT).host_pointer(ptr.cast());
            let mem = d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(len as u64).memory_type_index(ty).push_next(&mut imp), None).map_err(err("vkAllocateMemory"))?;
            d.bind_buffer_memory(buf, mem, 0).map_err(err("vkBindBufferMemory"))?;
            Ok(Buffer { ptr, len, buf, mem })
        }
    }

    /// Copia entre buffers (blit de Metal: copyFromBuffer:...toBuffer:...).
    pub fn copiar_buffer(&self, de: &Buffer, de_off: u64, a: &Buffer, a_off: u64, len: u64) -> Res<()> {
        let r = [vk::BufferCopy::default().src_offset(de_off).dst_offset(a_off).size(len)];
        self.una_vez(|d, cb| {
            // SAFETY: objetos válidos durante la grabación.
            unsafe { d.cmd_copy_buffer(cb, de.buf, a.buf, &r) }
        })
    }

    /// Sampler para los samplers constantes de los shaders (lineal y sujeto al borde) mientras su estado de 64 bits no
    /// se decodifique; uno por motor.
    pub fn sampler_fijo(&self) -> Res<vk::Sampler> {
        if let Some(s) = self.fijo.get() {
            return Ok(*s);
        }
        let s = self.sampler(true)?;
        let _ = self.fijo.set(s);
        Ok(s)
    }

    /// Alineación que piden los desplazamientos de un storage buffer.
    pub fn alineacion_buffer(&self) -> u64 {
        self.gpu.props.limits.min_storage_buffer_offset_alignment
    }

    /// Buffer nuevo en un memfd propio (para pruebas).
    pub fn buffer(&self, len: usize) -> Res<Buffer> {
        let len = len.next_multiple_of(4096).max(4096);
        // SAFETY: llamadas al sistema con argumentos válidos.
        let ptr = unsafe {
            let fd = libc::memfd_create(c"tahoe-buffer".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 || libc::ftruncate(fd, len as libc::off_t) != 0 {
                return Err("memfd".into());
            }
            let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            libc::close(fd);
            if p == libc::MAP_FAILED {
                return Err("mmap".into());
            }
            p.cast::<u8>()
        };
        // SAFETY: proyección propia de len bytes que el Buffer no libera (vive lo que el proceso; es para pruebas).
        unsafe { self.buffer_en(ptr, len) }
    }

    pub fn textura(&self, ancho: u32, alto: u32, formato: vk::Format) -> Res<Textura> {
        self.textura_forma(Forma::D2, ancho, alto, 1, 1, formato)
    }

    /// Textura de cualquier forma: `prof` para 3D, `capas` para arrays (en cubos, número de cubos).
    pub fn textura_forma(&self, forma: Forma, ancho: u32, alto: u32, prof: u32, capas: u32, formato: vk::Format) -> Res<Textura> {
        self.textura_completa(forma, ancho, alto, prof, capas, 1, formato)
    }

    /// Usos que la GPU admite para un formato con tiling óptimo (sampled, storage, color attachment).
    fn usos_formato(&self, formato: vk::Format) -> vk::ImageUsageFlags {
        // SAFETY: dispositivo físico de esta instancia.
        let p = unsafe { self.gpu.instance.get_physical_device_format_properties(self.gpu.phys, formato) }.optimal_tiling_features;
        let mut u = vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST;
        if p.contains(vk::FormatFeatureFlags::SAMPLED_IMAGE) {
            u |= vk::ImageUsageFlags::SAMPLED;
        }
        if p.contains(vk::FormatFeatureFlags::STORAGE_IMAGE) {
            u |= vk::ImageUsageFlags::STORAGE;
        }
        if p.contains(vk::FormatFeatureFlags::COLOR_ATTACHMENT) {
            u |= vk::ImageUsageFlags::COLOR_ATTACHMENT;
        }
        if p.contains(vk::FormatFeatureFlags::DEPTH_STENCIL_ATTACHMENT) {
            u |= vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT;
        }
        u
    }

    /// Textura de cualquier forma y con `niveles` niveles de mipmap; los usos, los que admita el formato.
    #[allow(clippy::too_many_arguments)]
    pub fn textura_completa(&self, forma: Forma, ancho: u32, alto: u32, prof: u32, capas: u32, niveles: u32, formato: vk::Format) -> Res<Textura> {
        let d = &self.gpu.device;
        let bpp = match formato {
            vk::Format::R32G32B32A32_SFLOAT | vk::Format::R32G32B32A32_UINT | vk::Format::R32G32B32A32_SINT => 16,
            vk::Format::R16G16B16A16_SFLOAT => 8,
            vk::Format::R8G8B8A8_UNORM | vk::Format::B8G8R8A8_UNORM | vk::Format::R32_SFLOAT | vk::Format::R32_UINT => 4,
            vk::Format::R16_SFLOAT => 2,
            vk::Format::R8_UNORM => 1,
            f => crate::formatos::bytes_de(f).ok_or_else(|| format!("formato {f:?} sin tamaño conocido"))?,
        };
        let (tipo, vista_t) = match forma {
            Forma::D1 => (vk::ImageType::TYPE_1D, vk::ImageViewType::TYPE_1D),
            Forma::D1Array => (vk::ImageType::TYPE_1D, vk::ImageViewType::TYPE_1D_ARRAY),
            Forma::D2 => (vk::ImageType::TYPE_2D, vk::ImageViewType::TYPE_2D),
            Forma::D2Array => (vk::ImageType::TYPE_2D, vk::ImageViewType::TYPE_2D_ARRAY),
            Forma::D3 => (vk::ImageType::TYPE_3D, vk::ImageViewType::TYPE_3D),
            Forma::Cubo => (vk::ImageType::TYPE_2D, vk::ImageViewType::CUBE),
            Forma::CuboArray => (vk::ImageType::TYPE_2D, vk::ImageViewType::CUBE_ARRAY),
        };
        let cubo = matches!(forma, Forma::Cubo | Forma::CuboArray);
        let alto = if matches!(forma, Forma::D1 | Forma::D1Array) { 1 } else { alto };
        let prof = if forma == Forma::D3 { prof.max(1) } else { 1 };
        let capas = if cubo { capas.max(1) * 6 } else if matches!(forma, Forma::D1Array | Forma::D2Array) { capas.max(1) } else { 1 };
        // SAFETY: objetos de este dispositivo.
        unsafe {
            let ci = vk::ImageCreateInfo::default()
                .flags(if cubo { vk::ImageCreateFlags::CUBE_COMPATIBLE } else { vk::ImageCreateFlags::empty() })
                .image_type(tipo)
                .format(formato)
                .extent(vk::Extent3D { width: ancho, height: alto, depth: prof })
                .mip_levels(niveles.max(1))
                .array_layers(capas)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(self.usos_formato(formato))
                .initial_layout(vk::ImageLayout::UNDEFINED);
            let image = d.create_image(&ci, None).map_err(err("vkCreateImage"))?;
            let req = d.get_image_memory_requirements(image);
            let ty = self.gpu.memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL).ok_or("sin memoria para la imagen")?;
            let mem = d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty), None).map_err(err("vkAllocateMemory"))?;
            d.bind_image_memory(image, mem, 0).map_err(err("vkBindImageMemory"))?;
            let aspecto = crate::formatos::aspecto(formato);
            let sr = vk::ImageSubresourceRange::default().aspect_mask(aspecto).level_count(niveles.max(1)).layer_count(capas);
            let vista = d.create_image_view(&vk::ImageViewCreateInfo::default().image(image).view_type(vista_t).format(formato).subresource_range(sr), None).map_err(err("vkCreateImageView"))?;
            let t = Textura { ancho, alto, prof, capas, formato, bpp, image, mem, vista };
            // todas las texturas viven en GENERAL (se leen, muestrean y escriben sin más transiciones)
            self.una_vez(|d, cb| {
                let b = [vk::ImageMemoryBarrier::default().image(image).subresource_range(sr).old_layout(vk::ImageLayout::UNDEFINED).new_layout(vk::ImageLayout::GENERAL).dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)];
                d.cmd_pipeline_barrier(cb, vk::PipelineStageFlags::TOP_OF_PIPE, vk::PipelineStageFlags::ALL_COMMANDS, vk::DependencyFlags::empty(), &[], &[], &b);
            })?;
            Ok(t)
        }
    }

    /// Copia de una región entre una textura y un buffer (subir: buffer → textura). `fila` en texeles (0: justa).
    #[allow(clippy::too_many_arguments)]
    pub fn copiar_region(&self, t: &Textura, b: &Buffer, b_off: u64, fila: u32, nivel: u32, capa: u32, origen: [i32; 3], tam: [u32; 3], subir: bool) -> Res<()> {
        let region = [vk::BufferImageCopy::default()
            .buffer_offset(b_off)
            .buffer_row_length(fila)
            .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(crate::formatos::aspecto(t.formato)).mip_level(nivel).base_array_layer(capa).layer_count(1))
            .image_offset(vk::Offset3D { x: origen[0], y: origen[1], z: origen[2] })
            .image_extent(vk::Extent3D { width: tam[0].max(1), height: tam[1].max(1), depth: tam[2].max(1) })];
        self.una_vez(|d, cb| {
            // SAFETY: objetos válidos durante la grabación.
            unsafe {
                if subir {
                    d.cmd_copy_buffer_to_image(cb, b.buf, t.image, vk::ImageLayout::GENERAL, &region);
                } else {
                    d.cmd_copy_image_to_buffer(cb, t.image, vk::ImageLayout::GENERAL, b.buf, &region);
                }
            }
        })
    }

    /// Sampler de un MTLSamplerDescriptor (ver protocolo::tipo::SAMPLER).
    #[allow(clippy::too_many_arguments)]
    pub fn sampler_desc(&self, min: u32, mag: u32, mip: u32, dir: [u32; 3], normalizadas: bool, aniso: u32, lod: [f32; 2], borde: u32) -> Res<vk::Sampler> {
        let filtro = |f: u32| if f == 1 { vk::Filter::LINEAR } else { vk::Filter::NEAREST };
        let a = |x: u32| crate::formatos::direccion(x);
        let aniso_ok = aniso > 1 && self.gpu.props.limits.max_sampler_anisotropy > 1.0;
        let mut ci = vk::SamplerCreateInfo::default()
            .min_filter(filtro(min))
            .mag_filter(filtro(mag))
            .mipmap_mode(if mip == 2 { vk::SamplerMipmapMode::LINEAR } else { vk::SamplerMipmapMode::NEAREST })
            .address_mode_u(a(dir[0]))
            .address_mode_v(a(dir[1]))
            .address_mode_w(a(dir[2]))
            .anisotropy_enable(aniso_ok)
            .max_anisotropy((aniso as f32).min(self.gpu.props.limits.max_sampler_anisotropy))
            .min_lod(lod[0])
            .max_lod(if mip == 0 { 0.25 } else { lod[1] })
            .unnormalized_coordinates(!normalizadas)
            .border_color(match borde {
                1 => vk::BorderColor::FLOAT_OPAQUE_BLACK,
                2 => vk::BorderColor::FLOAT_OPAQUE_WHITE,
                _ => vk::BorderColor::FLOAT_TRANSPARENT_BLACK,
            });
        if !normalizadas {
            // coordenadas sin normalizar: Vulkan pide un solo nivel y sujetar al borde
            ci = ci.min_lod(0.0).max_lod(0.0).mipmap_mode(vk::SamplerMipmapMode::NEAREST).anisotropy_enable(false).address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE).address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE);
        }
        // SAFETY: dispositivo válido.
        unsafe { self.gpu.device.create_sampler(&ci, None) }.map_err(err("vkCreateSampler"))
    }

    /// Copia entre una textura y un buffer (subir: buffer → textura).
    pub fn copiar(&self, t: &Textura, b: &Buffer, subir: bool) -> Res<()> {
        let region = [vk::BufferImageCopy::default()
            .image_subresource(vk::ImageSubresourceLayers::default().aspect_mask(vk::ImageAspectFlags::COLOR).layer_count(t.capas))
            .image_extent(vk::Extent3D { width: t.ancho, height: t.alto, depth: t.prof })];
        self.una_vez(|d, cb| {
            // SAFETY: objetos válidos durante la grabación.
            unsafe {
                if subir {
                    d.cmd_copy_buffer_to_image(cb, b.buf, t.image, vk::ImageLayout::GENERAL, &region);
                } else {
                    d.cmd_copy_image_to_buffer(cb, t.image, vk::ImageLayout::GENERAL, b.buf, &region);
                }
            }
        })
    }

    /// Sampler: lineal y con las coordenadas sujetas al borde (el estado de los samplers constantes de Metal aún no se
    /// interpreta).
    pub fn sampler(&self, lineal: bool) -> Res<vk::Sampler> {
        let f = if lineal { vk::Filter::LINEAR } else { vk::Filter::NEAREST };
        let ci = vk::SamplerCreateInfo::default()
            .mag_filter(f)
            .min_filter(f)
            .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
            .max_lod(vk::LOD_CLAMP_NONE);
        // SAFETY: dispositivo válido.
        unsafe { self.gpu.device.create_sampler(&ci, None) }.map_err(err("vkCreateSampler"))
    }

    pub fn pipeline_computo(&self, t: &Traduccion, grupo: [u32; 3]) -> Res<PipelineComputo> {
        let d = &self.gpu.device;
        let enlaces: Vec<(u32, vk::DescriptorType)> = t.enlaces.iter().map(|e| (e.binding, tipo_descriptor(&e.recurso))).collect();
        let binds: Vec<vk::DescriptorSetLayoutBinding> =
            enlaces.iter().map(|&(b, ty)| vk::DescriptorSetLayoutBinding::default().binding(b).descriptor_type(ty).descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE)).collect();
        // SAFETY: objetos de este dispositivo; el SPIR-V viene del traductor (validado con spirv-val en las pruebas).
        unsafe {
            let set_layout = d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&binds), None).map_err(err("dsl"))?;
            let sls = [set_layout];
            let pc = [vk::PushConstantRange::default().stage_flags(vk::ShaderStageFlags::COMPUTE).offset(0).size(16)];
            let layout = d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&sls).push_constant_ranges(&pc), None).map_err(err("pl"))?;
            let sm = d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&t.spirv), None).map_err(err("vkCreateShaderModule"))?;
            let mapas: Vec<vk::SpecializationMapEntry> = (0..3).map(|k| vk::SpecializationMapEntry::default().constant_id(k).offset(k * 4).size(4)).collect();
            let datos: Vec<u8> = grupo.iter().flat_map(|x| x.to_le_bytes()).collect();
            let spec = vk::SpecializationInfo::default().map_entries(&mapas).data(&datos);
            let mut stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE).module(sm).name(c"main").specialization_info(&spec);
            // SIMD-groups de Metal: 32 hilos
            let mut req = vk::PipelineShaderStageRequiredSubgroupSizeCreateInfo::default().required_subgroup_size(32);
            if t.subgrupos && self.gpu.subgrupo_fijo {
                stage = stage.push_next(&mut req);
            }
            let ci = [vk::ComputePipelineCreateInfo::default().stage(stage).layout(layout)];
            let r = d.create_compute_pipelines(vk::PipelineCache::null(), &ci, None);
            d.destroy_shader_module(sm, None);
            let pipeline = r.map_err(|(_, e)| format!("vkCreateComputePipelines: {e:?}"))?[0];
            Ok(PipelineComputo { pipeline, layout, set_layout, enlaces })
        }
    }

    /// Despacha `grid` hilos (como dispatchThreads de Metal; los grupos de los bordes se recortan en el shader).
    pub fn despachar(&self, p: &PipelineComputo, recursos: &HashMap<u32, Enlazado>, grid: [u32; 3], grupo: [u32; 3]) -> Res<()> {
        let d = &self.gpu.device;
        // SAFETY: objetos de este dispositivo; los recursos viven durante la llamada (se espera al final).
        unsafe {
            let mut tam: HashMap<vk::DescriptorType, u32> = HashMap::new();
            for &(_, t) in &p.enlaces {
                *tam.entry(t).or_default() += 1;
            }
            let sizes: Vec<vk::DescriptorPoolSize> = tam.iter().map(|(&t, &n)| vk::DescriptorPoolSize::default().ty(t).descriptor_count(n)).collect();
            let pool = d.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(if sizes.is_empty() { &[vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLER, descriptor_count: 1 }] } else { &sizes }), None).map_err(err("pool"))?;
            let sls = [p.set_layout];
            let set = d.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(pool).set_layouts(&sls)).map_err(err("set"))?[0];
            let mut binfos = Vec::new();
            let mut iinfos = Vec::new();
            for &(b, ty) in &p.enlaces {
                match recursos.get(&b) {
                    Some(Enlazado::Buffer(buf)) => binfos.push((b, ty, vk::DescriptorBufferInfo::default().buffer(buf.buf).range(vk::WHOLE_SIZE))),
                    Some(Enlazado::BufferDesde(buf, off)) => binfos.push((b, ty, vk::DescriptorBufferInfo::default().buffer(buf.buf).offset(*off).range(vk::WHOLE_SIZE))),
                    Some(Enlazado::Textura(t)) => iinfos.push((b, ty, vk::DescriptorImageInfo::default().image_view(t.vista).image_layout(vk::ImageLayout::GENERAL))),
                    Some(Enlazado::Sampler(s)) => iinfos.push((b, ty, vk::DescriptorImageInfo::default().sampler(*s))),
                    None => return Err(format!("falta el recurso del binding {b}")),
                }
            }
            let mut writes = Vec::new();
            for (b, ty, info) in &binfos {
                writes.push(vk::WriteDescriptorSet::default().dst_set(set).dst_binding(*b).descriptor_type(*ty).buffer_info(std::slice::from_ref(info)));
            }
            for (b, ty, info) in &iinfos {
                writes.push(vk::WriteDescriptorSet::default().dst_set(set).dst_binding(*b).descriptor_type(*ty).image_info(std::slice::from_ref(info)));
            }
            d.update_descriptor_sets(&writes, &[]);
            let grupos = [grid[0].div_ceil(grupo[0]), grid[1].div_ceil(grupo[1]), grid[2].div_ceil(grupo[2])];
            let pc: Vec<u8> = grid.iter().chain(&[0u32]).flat_map(|x| x.to_le_bytes()).collect();
            let r = self.una_vez(|d, cb| {
                d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, p.pipeline);
                d.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::COMPUTE, p.layout, 0, &[set], &[]);
                d.cmd_push_constants(cb, p.layout, vk::ShaderStageFlags::COMPUTE, 0, &pc);
                d.cmd_dispatch(cb, grupos[0], grupos[1], grupos[2]);
                let mb = [vk::MemoryBarrier::default().src_access_mask(vk::AccessFlags::SHADER_WRITE).dst_access_mask(vk::AccessFlags::HOST_READ | vk::AccessFlags::TRANSFER_READ)];
                d.cmd_pipeline_barrier(cb, vk::PipelineStageFlags::COMPUTE_SHADER, vk::PipelineStageFlags::HOST | vk::PipelineStageFlags::TRANSFER, vk::DependencyFlags::empty(), &mb, &[], &[]);
            });
            d.destroy_descriptor_pool(pool, None);
            r
        }
    }
}

impl Drop for Motor {
    fn drop(&mut self) {
        // SAFETY: se espera a que la GPU termine antes de destruir.
        unsafe {
            let _ = self.gpu.device.device_wait_idle();
            self.gpu.device.destroy_command_pool(self.pool, None);
        }
    }
}

impl Buffer {
    pub fn bytes(&self) -> &mut [u8] {
        // SAFETY: proyección de len bytes que vive lo que el Buffer.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

#[allow(dead_code)]
impl Textura {
    pub fn destruir(self, m: &Motor) {
        // SAFETY: nadie usa ya la textura.
        unsafe {
            m.gpu.device.destroy_image_view(self.vista, None);
            m.gpu.device.destroy_image(self.image, None);
            m.gpu.device.free_memory(self.mem, None);
        }
    }
}

#[allow(dead_code)]
impl Buffer {
    pub fn destruir(self, m: &Motor) {
        // SAFETY: nadie usa ya el buffer.
        unsafe {
            m.gpu.device.destroy_buffer(self.buf, None);
            m.gpu.device.free_memory(self.mem, None);
        }
    }
}
