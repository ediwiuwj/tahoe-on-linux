//! `tahoe-gpu prueba`: un kernel de cómputo sobre memoria compartida (memfd importado con
//! VK_EXT_external_memory_host), comprobando el resultado desde la CPU en la misma memoria.

use std::time::Instant;

use ash::vk;

use crate::shaders;
use crate::vk::{err, Gpu, Res};

const N: usize = 1 << 20; // un millón de enteros (4 MB)

/// Memoria compartida: un memfd proyectado (así llegará la memoria de un MTLBuffer desde el proceso de macOS).
struct Compartida {
    ptr: *mut u32,
    len: usize,
}

impl Compartida {
    fn new(len: usize) -> Res<Compartida> {
        // SAFETY: llamadas al sistema con argumentos válidos; el descriptor se cierra tras proyectar.
        unsafe {
            let fd = libc::memfd_create(c"tahoe-gpu-prueba".as_ptr(), libc::MFD_CLOEXEC);
            if fd < 0 || libc::ftruncate(fd, len as libc::off_t) != 0 {
                return Err("memfd_create/ftruncate".into());
            }
            let p = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, fd, 0);
            libc::close(fd);
            if p == libc::MAP_FAILED {
                return Err("mmap".into());
            }
            Ok(Compartida { ptr: p.cast(), len })
        }
    }
    fn datos(&mut self) -> &mut [u32] {
        // SAFETY: proyección propia de len bytes, alineada a página.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len / 4) }
    }
}

impl Drop for Compartida {
    fn drop(&mut self) {
        // SAFETY: proyección creada en new y que ya nadie usa.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

pub fn prueba(g: &Gpu) -> Res<()> {
    let hm = g.host_memory.as_ref().ok_or("la GPU no tiene VK_EXT_external_memory_host")?;
    let d = &g.device;
    let size = (N * 4) as vk::DeviceSize;
    let mut mem = Compartida::new(N * 4)?;
    for (i, v) in mem.datos().iter_mut().enumerate() {
        *v = i as u32;
    }
    // SAFETY: todas las llamadas usan objetos de este dispositivo, creados aquí y destruidos al final; la memoria
    // importada (mem) vive más que el buffer y la GPU termina (fence) antes de leerla.
    unsafe {
        let hp = {
            let mut p = vk::MemoryHostPointerPropertiesEXT::default();
            (hm.fp().get_memory_host_pointer_properties_ext)(d.handle(), vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT, mem.ptr.cast(), &mut p)
                .result()
                .map_err(err("vkGetMemoryHostPointerPropertiesEXT"))?;
            p
        };
        let mut ext = vk::ExternalMemoryBufferCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT);
        let bci = vk::BufferCreateInfo::default().size(size).usage(vk::BufferUsageFlags::STORAGE_BUFFER).push_next(&mut ext);
        let buf = d.create_buffer(&bci, None).map_err(err("vkCreateBuffer"))?;
        let req = d.get_buffer_memory_requirements(buf);
        let ty = g
            .memory_type(req.memory_type_bits & hp.memory_type_bits, vk::MemoryPropertyFlags::HOST_VISIBLE)
            .ok_or("ningún tipo de memoria admite la memoria importada")?;
        let mut imp = vk::ImportMemoryHostPointerInfoEXT::default()
            .handle_type(vk::ExternalMemoryHandleTypeFlags::HOST_ALLOCATION_EXT)
            .host_pointer(mem.ptr.cast());
        let mai = vk::MemoryAllocateInfo::default().allocation_size(size).memory_type_index(ty).push_next(&mut imp);
        let dm = d.allocate_memory(&mai, None).map_err(err("vkAllocateMemory (memoria importada)"))?;
        d.bind_buffer_memory(buf, dm, 0).map_err(err("vkBindBufferMemory"))?;

        let code = shaders::doble_mas_uno();
        let sm = d.create_shader_module(&vk::ShaderModuleCreateInfo::default().code(&code), None).map_err(err("vkCreateShaderModule"))?;
        let bind = [vk::DescriptorSetLayoutBinding::default()
            .binding(0)
            .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1)
            .stage_flags(vk::ShaderStageFlags::COMPUTE)];
        let dsl = d.create_descriptor_set_layout(&vk::DescriptorSetLayoutCreateInfo::default().bindings(&bind), None).map_err(err("dsl"))?;
        let dsls = [dsl];
        let pl = d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default().set_layouts(&dsls), None).map_err(err("pl"))?;
        let stage = vk::PipelineShaderStageCreateInfo::default().stage(vk::ShaderStageFlags::COMPUTE).module(sm).name(c"main");
        let cpi = [vk::ComputePipelineCreateInfo::default().stage(stage).layout(pl)];
        let pipe = d.create_compute_pipelines(vk::PipelineCache::null(), &cpi, None).map_err(|(_, e)| format!("pipeline: {e:?}"))?[0];
        let sizes = [vk::DescriptorPoolSize::default().ty(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(1)];
        let pool = d.create_descriptor_pool(&vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(&sizes), None).map_err(err("pool"))?;
        let set = d.allocate_descriptor_sets(&vk::DescriptorSetAllocateInfo::default().descriptor_pool(pool).set_layouts(&dsls)).map_err(err("set"))?[0];
        let bi = [vk::DescriptorBufferInfo::default().buffer(buf).offset(0).range(size)];
        let w = [vk::WriteDescriptorSet::default().dst_set(set).dst_binding(0).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&bi)];
        d.update_descriptor_sets(&w, &[]);

        let cp = d.create_command_pool(&vk::CommandPoolCreateInfo::default().queue_family_index(g.queue_family), None).map_err(err("cpool"))?;
        let cb = d.allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(cp).command_buffer_count(1)).map_err(err("cb"))?[0];
        d.begin_command_buffer(cb, &vk::CommandBufferBeginInfo::default()).map_err(err("begin"))?;
        d.cmd_bind_pipeline(cb, vk::PipelineBindPoint::COMPUTE, pipe);
        d.cmd_bind_descriptor_sets(cb, vk::PipelineBindPoint::COMPUTE, pl, 0, &[set], &[]);
        d.cmd_dispatch(cb, (N / 64) as u32, 1, 1);
        let bar = [vk::BufferMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::SHADER_WRITE)
            .dst_access_mask(vk::AccessFlags::HOST_READ)
            .buffer(buf)
            .size(vk::WHOLE_SIZE)];
        d.cmd_pipeline_barrier(cb, vk::PipelineStageFlags::COMPUTE_SHADER, vk::PipelineStageFlags::HOST, vk::DependencyFlags::empty(), &[], &bar, &[]);
        d.end_command_buffer(cb).map_err(err("end"))?;
        let fence = d.create_fence(&vk::FenceCreateInfo::default(), None).map_err(err("fence"))?;
        let cbs = [cb];
        let t0 = Instant::now();
        d.queue_submit(g.queue, &[vk::SubmitInfo::default().command_buffers(&cbs)], fence).map_err(err("vkQueueSubmit"))?;
        d.wait_for_fences(&[fence], true, 5_000_000_000).map_err(err("vkWaitForFences"))?;
        let dt = t0.elapsed();

        let malos = mem.datos().iter().enumerate().filter(|&(i, &v)| v != (i as u32).wrapping_mul(2).wrapping_add(1)).count();
        println!("{}: {} enteros en memoria compartida, {:.2} ms en la GPU, {} incorrectos", g.name(), N, dt.as_secs_f64() * 1e3, malos);

        d.destroy_fence(fence, None);
        d.destroy_command_pool(cp, None);
        d.destroy_descriptor_pool(pool, None);
        d.destroy_pipeline(pipe, None);
        d.destroy_pipeline_layout(pl, None);
        d.destroy_descriptor_set_layout(dsl, None);
        d.destroy_shader_module(sm, None);
        d.destroy_buffer(buf, None);
        d.free_memory(dm, None);
        if malos != 0 {
            return Err(format!("{malos} resultados incorrectos"));
        }
    }
    Ok(())
}
