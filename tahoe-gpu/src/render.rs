//! Render en el motor: pipelines de render (vertex + fragment traducidos de AIR) y pases de render con sus dibujos,
//! lo que hace un MTLRenderCommandEncoder.
//!
//! Correspondencias con Metal:
//! - recursos: los del vertex shader en el descriptor set 0 y los del fragment en el 1 (en Metal el buffer 0 de cada
//!   etapa es distinto; el traductor los pone en el set de `Opciones::set`);
//! - atributos de vértice (`[[stage_in]]`): location = índice de atributo de Metal; los «vertex buffers» de Vulkan son
//!   los buffers de Metal con su índice;
//! - el eje Y: la normalizada de Metal va hacia arriba y la de Vulkan hacia abajo; se usa un viewport de altura negativa
//!   (VK_KHR_maintenance1, en Vulkan 1.1), así que coordenadas de textura y de framebuffer coinciden con Metal;
//! - la topología va en el pipeline en Vulkan 1.2: uno por clase de topología (cacheado por quien lo use).

use std::collections::HashMap;

use ash::vk;

use crate::air::spirv::{Recurso, Traduccion};
use crate::motor::{Buffer, Enlazado, Motor, Textura};
use crate::vk::{err, Res};

/// Un atributo de vértice: location (índice de Metal), buffer (binding de vértices), formato y desplazamiento.
#[derive(Clone, Copy, Debug)]
pub struct Atributo {
    pub location: u32,
    pub buffer: u32,
    pub formato: vk::Format,
    pub offset: u32,
}

/// Un buffer de vértices: binding (índice de Metal), paso y si avanza por instancia.
#[derive(Clone, Copy, Debug)]
pub struct EnlaceVertices {
    pub buffer: u32,
    pub paso: u32,
    pub por_instancia: bool,
}

/// Mezcla de un color (MTLRenderPipelineColorAttachmentDescriptor).
#[derive(Clone, Copy, Debug)]
pub struct Mezcla {
    pub activa: bool,
    pub src_color: vk::BlendFactor,
    pub dst_color: vk::BlendFactor,
    pub op_color: vk::BlendOp,
    pub src_alfa: vk::BlendFactor,
    pub dst_alfa: vk::BlendFactor,
    pub op_alfa: vk::BlendOp,
    pub mascara: vk::ColorComponentFlags,
}

impl Default for Mezcla {
    fn default() -> Self {
        Mezcla {
            activa: false,
            src_color: vk::BlendFactor::ONE,
            dst_color: vk::BlendFactor::ZERO,
            op_color: vk::BlendOp::ADD,
            src_alfa: vk::BlendFactor::ONE,
            dst_alfa: vk::BlendFactor::ZERO,
            op_alfa: vk::BlendOp::ADD,
            mascara: vk::ColorComponentFlags::RGBA,
        }
    }
}

pub struct DescRender<'a> {
    pub vertex: &'a Traduccion,
    pub fragment: Option<&'a Traduccion>,
    pub atributos: Vec<Atributo>,
    pub enlaces_vertices: Vec<EnlaceVertices>,
    /// formato y mezcla de cada color (índice = render target)
    pub colores: Vec<(vk::Format, Mezcla)>,
    pub topologia: vk::PrimitiveTopology,
}

pub struct PipelineRender {
    pipeline: vk::Pipeline,
    layout: vk::PipelineLayout,
    sets: [vk::DescriptorSetLayout; 2],
    enlaces: [Vec<(u32, vk::DescriptorType)>; 2],
    pase: vk::RenderPass,
}

fn tipo_descriptor(r: &Recurso) -> vk::DescriptorType {
    match r {
        Recurso::Buffer { .. } => vk::DescriptorType::STORAGE_BUFFER,
        Recurso::Textura { .. } => vk::DescriptorType::SAMPLED_IMAGE,
        Recurso::TexturaEscritura { .. } => vk::DescriptorType::STORAGE_IMAGE,
        Recurso::Sampler { .. } | Recurso::SamplerFijo { .. } => vk::DescriptorType::SAMPLER,
    }
}

/// Cómo empieza un color en un pase (MTLLoadAction).
#[derive(Clone, Copy, Debug)]
pub enum Carga {
    Cargar,
    Limpiar([f32; 4]),
    DaIgual,
}

pub struct ColorPase<'a> {
    pub textura: &'a Textura,
    pub carga: Carga,
    pub guardar: bool,
}

pub struct Dibujo<'a> {
    pub pipeline: &'a PipelineRender,
    /// recursos por binding del vertex (set 0) y del fragment (set 1)
    pub recursos: [HashMap<u32, Enlazado<'a>>; 2],
    pub vertices: Vec<(u32, &'a Buffer, u64)>,
    /// buffer de índices, desplazamiento y si son de 32 bits
    pub indices: Option<(&'a Buffer, u64, bool)>,
    pub primero: u32,
    pub cuenta: u32,
    pub instancias: u32,
    pub primera_instancia: u32,
    pub base_vertice: i32,
    /// x, y, ancho, alto, profundidad mínima y máxima (como MTLViewport)
    pub viewport: [f32; 6],
    pub tijera: Option<[i32; 4]>,
    pub cull: vk::CullModeFlags,
    pub horario: bool,
}

fn pase_compatible(d: &ash::Device, formatos: &[vk::Format], cargas: &[vk::AttachmentLoadOp], guardar: &[bool]) -> Res<vk::RenderPass> {
    let att: Vec<vk::AttachmentDescription> = formatos
        .iter()
        .enumerate()
        .map(|(i, &f)| {
            vk::AttachmentDescription::default()
                .format(f)
                .samples(vk::SampleCountFlags::TYPE_1)
                .load_op(cargas.get(i).copied().unwrap_or(vk::AttachmentLoadOp::LOAD))
                .store_op(if guardar.get(i).copied().unwrap_or(true) { vk::AttachmentStoreOp::STORE } else { vk::AttachmentStoreOp::DONT_CARE })
                .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
                .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
                .initial_layout(vk::ImageLayout::GENERAL)
                .final_layout(vk::ImageLayout::GENERAL)
        })
        .collect();
    let refs: Vec<vk::AttachmentReference> = (0..formatos.len() as u32).map(|i| vk::AttachmentReference::default().attachment(i).layout(vk::ImageLayout::GENERAL)).collect();
    let sub = [vk::SubpassDescription::default().pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS).color_attachments(&refs)];
    // SAFETY: estructuras válidas de este dispositivo.
    unsafe { d.create_render_pass(&vk::RenderPassCreateInfo::default().attachments(&att).subpasses(&sub), None) }.map_err(err("vkCreateRenderPass"))
}

impl Motor {
    pub fn pipeline_render(&self, desc: &DescRender) -> Res<PipelineRender> {
        let d = &self.gpu.device;
        let etapas = [Some(desc.vertex), desc.fragment];
        let flags = [vk::ShaderStageFlags::VERTEX, vk::ShaderStageFlags::FRAGMENT];
        // SAFETY: objetos de este dispositivo; el SPIR-V viene del traductor.
        unsafe {
            let mut sets = [vk::DescriptorSetLayout::null(); 2];
            let mut enlaces: [Vec<(u32, vk::DescriptorType)>; 2] = [Vec::new(), Vec::new()];
            for k in 0..2 {
                if let Some(t) = etapas[k] {
                    enlaces[k] = t.enlaces.iter().map(|e| (e.binding, tipo_descriptor(&e.recurso))).collect();
                }
                let binds: Vec<vk::DescriptorSetLayoutBinding> =
                    enlaces[k].iter().map(|&(b, ty)| vk::DescriptorSetLayoutBinding::default().binding(b).descriptor_type(ty).descriptor_count(1).stage_flags(flags[k])).collect();
                sets[k] = d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&binds), None).map_err(err("dsl"))?;
            }
            let layout = d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&sets), None).map_err(err("pl"))?;
            let formatos: Vec<vk::Format> = desc.colores.iter().map(|c| c.0).collect();
            let pase = pase_compatible(d, &formatos, &[], &[])?;
            let mut modulos = Vec::new();
            let mut stages = Vec::new();
            for k in 0..2 {
                if let Some(t) = etapas[k] {
                    let sm = d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&t.spirv), None).map_err(err("vkCreateShaderModule"))?;
                    modulos.push(sm);
                    stages.push(vk::PipelineShaderStageCreateInfo::default().stage(flags[k]).module(sm).name(c"main"));
                }
            }
            let vb: Vec<vk::VertexInputBindingDescription> = desc
                .enlaces_vertices
                .iter()
                .map(|e| vk::VertexInputBindingDescription::default().binding(e.buffer).stride(e.paso).input_rate(if e.por_instancia { vk::VertexInputRate::INSTANCE } else { vk::VertexInputRate::VERTEX }))
                .collect();
            let va: Vec<vk::VertexInputAttributeDescription> =
                desc.atributos.iter().map(|a| vk::VertexInputAttributeDescription::default().location(a.location).binding(a.buffer).format(a.formato).offset(a.offset)).collect();
            let vi = vk::PipelineVertexInputStateCreateInfo::default().vertex_binding_descriptions(&vb).vertex_attribute_descriptions(&va);
            let ia = vk::PipelineInputAssemblyStateCreateInfo::default().topology(desc.topologia);
            let vp = vk::PipelineViewportStateCreateInfo::default().viewport_count(1).scissor_count(1);
            let rs = vk::PipelineRasterizationStateCreateInfo::default().polygon_mode(vk::PolygonMode::FILL).line_width(1.0);
            let ms = vk::PipelineMultisampleStateCreateInfo::default().rasterization_samples(vk::SampleCountFlags::TYPE_1);
            let cb: Vec<vk::PipelineColorBlendAttachmentState> = desc
                .colores
                .iter()
                .map(|(_, m)| {
                    vk::PipelineColorBlendAttachmentState::default()
                        .blend_enable(m.activa)
                        .src_color_blend_factor(m.src_color)
                        .dst_color_blend_factor(m.dst_color)
                        .color_blend_op(m.op_color)
                        .src_alpha_blend_factor(m.src_alfa)
                        .dst_alpha_blend_factor(m.dst_alfa)
                        .alpha_blend_op(m.op_alfa)
                        .color_write_mask(m.mascara)
                })
                .collect();
            let cbs = vk::PipelineColorBlendStateCreateInfo::default().attachments(&cb);
            let din = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR, vk::DynamicState::BLEND_CONSTANTS];
            let ds = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&din);
            let ci = [vk::GraphicsPipelineCreateInfo::default()
                .stages(&stages)
                .vertex_input_state(&vi)
                .input_assembly_state(&ia)
                .viewport_state(&vp)
                .rasterization_state(&rs)
                .multisample_state(&ms)
                .color_blend_state(&cbs)
                .dynamic_state(&ds)
                .layout(layout)
                .render_pass(pase)
                .subpass(0)];
            let r = d.create_graphics_pipelines(vk::PipelineCache::null(), &ci, None);
            for sm in modulos {
                d.destroy_shader_module(sm, None);
            }
            let pipeline = r.map_err(|(_, e)| format!("vkCreateGraphicsPipelines: {e:?}"))?[0];
            Ok(PipelineRender { pipeline, layout, sets, enlaces, pase })
        }
    }

    /// Un pase de render con sus dibujos (un MTLRenderCommandEncoder entero), con espera.
    pub fn pase_render(&self, colores: &[ColorPase], dibujos: &[Dibujo]) -> Res<()> {
        let d = &self.gpu.device;
        let t0 = colores.first().ok_or("pase sin colores")?.textura;
        let (ancho, alto) = (t0.ancho, t0.alto);
        // SAFETY: objetos de este dispositivo; todo vive hasta después de la espera.
        unsafe {
            let formatos: Vec<vk::Format> = colores.iter().map(|c| c.textura.formato).collect();
            let cargas: Vec<vk::AttachmentLoadOp> = colores
                .iter()
                .map(|c| match c.carga {
                    Carga::Cargar => vk::AttachmentLoadOp::LOAD,
                    Carga::Limpiar(_) => vk::AttachmentLoadOp::CLEAR,
                    Carga::DaIgual => vk::AttachmentLoadOp::DONT_CARE,
                })
                .collect();
            let guardar: Vec<bool> = colores.iter().map(|c| c.guardar).collect();
            let pase = pase_compatible(d, &formatos, &cargas, &guardar)?;
            let vistas: Vec<vk::ImageView> = colores.iter().map(|c| c.textura.vista).collect();
            let fb = d.create_framebuffer(&vk::FramebufferCreateInfo::default().render_pass(pase).attachments(&vistas).width(ancho).height(alto).layers(1), None).map_err(err("vkCreateFramebuffer"))?;
            let limpiar: Vec<vk::ClearValue> = colores
                .iter()
                .map(|c| vk::ClearValue { color: vk::ClearColorValue { float32: if let Carga::Limpiar(v) = c.carga { v } else { [0.0; 4] } } })
                .collect();
            // descriptores: un set por etapa y dibujo
            let mut tam: HashMap<vk::DescriptorType, u32> = HashMap::new();
            for dib in dibujos {
                for e in dib.pipeline.enlaces.iter().flatten() {
                    *tam.entry(e.1).or_default() += 1;
                }
            }
            let sizes: Vec<vk::DescriptorPoolSize> = tam.iter().map(|(&t, &n)| vk::DescriptorPoolSize::default().ty(t).descriptor_count(n)).collect();
            let pool = d
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default().max_sets((dibujos.len() * 2).max(1) as u32).pool_sizes(if sizes.is_empty() { &[vk::DescriptorPoolSize { ty: vk::DescriptorType::SAMPLER, descriptor_count: 1 }] } else { &sizes }),
                    None,
                )
                .map_err(err("pool"))?;
            let mut sets_por_dibujo = Vec::new();
            for dib in dibujos {
                let sets = d.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(pool).set_layouts(&dib.pipeline.sets)).map_err(err("set"))?;
                for k in 0..2 {
                    let mut binfos = Vec::new();
                    let mut iinfos = Vec::new();
                    for &(b, ty) in &dib.pipeline.enlaces[k] {
                        match dib.recursos[k].get(&b) {
                            Some(Enlazado::Buffer(buf)) => binfos.push((b, ty, vk::DescriptorBufferInfo::default().buffer(buf.buf).range(vk::WHOLE_SIZE))),
                            Some(Enlazado::BufferDesde(buf, off)) => binfos.push((b, ty, vk::DescriptorBufferInfo::default().buffer(buf.buf).offset(*off).range(vk::WHOLE_SIZE))),
                            Some(Enlazado::Textura(t)) => iinfos.push((b, ty, vk::DescriptorImageInfo::default().image_view(t.vista).image_layout(vk::ImageLayout::GENERAL))),
                            Some(Enlazado::Sampler(s)) => iinfos.push((b, ty, vk::DescriptorImageInfo::default().sampler(*s))),
                            None => {
                                d.destroy_descriptor_pool(pool, None);
                                d.destroy_framebuffer(fb, None);
                                d.destroy_render_pass(pase, None);
                                return Err(format!("falta el recurso del binding {b} ({})", if k == 0 { "vertex" } else { "fragment" }));
                            }
                        }
                    }
                    let mut writes = Vec::new();
                    for (b, ty, info) in &binfos {
                        writes.push(vk::WriteDescriptorSet::default().dst_set(sets[k]).dst_binding(*b).descriptor_type(*ty).buffer_info(std::slice::from_ref(info)));
                    }
                    for (b, ty, info) in &iinfos {
                        writes.push(vk::WriteDescriptorSet::default().dst_set(sets[k]).dst_binding(*b).descriptor_type(*ty).image_info(std::slice::from_ref(info)));
                    }
                    d.update_descriptor_sets(&writes, &[]);
                }
                sets_por_dibujo.push(sets);
            }
            let r = self.una_vez(|d, cb| {
                let bi = vk::RenderPassBeginInfo::default().render_pass(pase).framebuffer(fb).render_area(vk::Rect2D { offset: vk::Offset2D::default(), extent: vk::Extent2D { width: ancho, height: alto } }).clear_values(&limpiar);
                d.cmd_begin_render_pass(cb, &bi, vk::SubpassContents::INLINE);
                for (dib, sets) in dibujos.iter().zip(&sets_por_dibujo) {
                    let p = dib.pipeline;
                    d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::GRAPHICS, p.pipeline);
                    d.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::GRAPHICS, p.layout, 0, sets, &[]);
                    let [x, y, w, h, zn, zf] = dib.viewport;
                    // Y de Metal hacia arriba: viewport con altura negativa
                    d.cmd_set_viewport(cb, 0, &[vk::Viewport { x, y: y + h, width: w, height: -h, min_depth: zn, max_depth: zf }]);
                    let t = dib.tijera.unwrap_or([0, 0, ancho as i32, alto as i32]);
                    d.cmd_set_scissor(cb, 0, &[vk::Rect2D { offset: vk::Offset2D { x: t[0].max(0), y: t[1].max(0) }, extent: vk::Extent2D { width: t[2].max(0) as u32, height: t[3].max(0) as u32 } }]);
                    for &(b, buf, off) in &dib.vertices {
                        d.cmd_bind_vertex_buffers(cb, b, &[buf.buf], &[off]);
                    }
                    match dib.indices {
                        Some((buf, off, de32)) => {
                            d.cmd_bind_index_buffer(cb, buf.buf, off, if de32 { vk::IndexType::UINT32 } else { vk::IndexType::UINT16 });
                            d.cmd_draw_indexed(cb, dib.cuenta, dib.instancias.max(1), dib.primero, dib.base_vertice, dib.primera_instancia);
                        }
                        None => d.cmd_draw(cb, dib.cuenta, dib.instancias.max(1), dib.primero, dib.primera_instancia),
                    }
                }
                d.cmd_end_render_pass(cb);
                let mb = [vk::MemoryBarrier::default().src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE).dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)];
                d.cmd_pipeline_barrier(cb, vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT, vk::PipelineStageFlags::ALL_COMMANDS, vk::DependencyFlags::empty(), &mb, &[], &[]);
            });
            d.destroy_descriptor_pool(pool, None);
            d.destroy_framebuffer(fb, None);
            d.destroy_render_pass(pase, None);
            r
        }
    }
}

impl PipelineRender {
    pub fn destruir(self, m: &Motor) {
        let d = &m.gpu.device;
        // SAFETY: ya no se usa (las órdenes se esperan al ejecutarse).
        unsafe {
            d.destroy_pipeline(self.pipeline, None);
            d.destroy_pipeline_layout(self.layout, None);
            for s in self.sets {
                d.destroy_descriptor_set_layout(s, None);
            }
            d.destroy_render_pass(self.pase, None);
        }
    }
}
