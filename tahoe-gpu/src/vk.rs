//! Dispositivo Vulkan: instancia, la GPU elegida (la primera física que no sea de CPU), una cola de gráficos y
//! cómputo, y las extensiones que usa tahoe-gpu si están (memoria del anfitrión importada).

use std::ffi::c_char;

use ash::vk;

pub type Res<T> = Result<T, String>;

pub fn err(que: &str) -> impl Fn(vk::Result) -> String + '_ {
    move |e| format!("{que}: {e:?}")
}

pub struct Gpu {
    pub _entry: ash::Entry,
    pub instance: ash::Instance,
    pub phys: vk::PhysicalDevice,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub queue_family: u32,
    pub props: vk::PhysicalDeviceProperties,
    pub mem_props: vk::PhysicalDeviceMemoryProperties,
    /// Extensiones de dispositivo disponibles (nombres).
    pub extensions: Vec<String>,
    /// VK_EXT_external_memory_host activada (MTLBuffer en memoria compartida sin copias).
    pub host_memory: Option<ash::ext::external_memory_host::Device>,
}

impl Gpu {
    pub fn new() -> Res<Gpu> {
        // SAFETY: carga libvulkan.so del sistema; las funciones se resuelven por el cargador de Vulkan.
        let entry = unsafe { ash::Entry::load() }.map_err(|e| format!("no se encuentra Vulkan (libvulkan.so): {e}"))?;
        let app = vk::ApplicationInfo::default()
            .application_name(c"tahoe-gpu")
            .engine_name(c"tahoe-run")
            .api_version(vk::API_VERSION_1_2);
        let ici = vk::InstanceCreateInfo::default().application_info(&app);
        // SAFETY: estructuras válidas que viven hasta el final de la llamada.
        let instance = unsafe { entry.create_instance(&ici, None) }.map_err(err("vkCreateInstance"))?;
        // SAFETY: instancia válida.
        let devs = unsafe { instance.enumerate_physical_devices() }.map_err(err("vkEnumeratePhysicalDevices"))?;
        let phys = devs
            .iter()
            .copied()
            // SAFETY: dispositivo físico de esta instancia.
            .find(|&d| unsafe { instance.get_physical_device_properties(d) }.device_type != vk::PhysicalDeviceType::CPU)
            .or_else(|| devs.first().copied())
            .ok_or("no hay ninguna GPU con Vulkan")?;
        // SAFETY: dispositivo físico de esta instancia.
        let (props, mem_props, families, ext_props) = unsafe {
            (
                instance.get_physical_device_properties(phys),
                instance.get_physical_device_memory_properties(phys),
                instance.get_physical_device_queue_family_properties(phys),
                instance.enumerate_device_extension_properties(phys).map_err(err("extensiones"))?,
            )
        };
        let extensions: Vec<String> = ext_props
            .iter()
            .map(|e| cstr(&e.extension_name))
            .collect();
        let queue_family = families
            .iter()
            .position(|f| f.queue_flags.contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE))
            .ok_or("la GPU no tiene una cola de gráficos y cómputo")? as u32;
        let prio = [1.0f32];
        let qci = [vk::DeviceQueueCreateInfo::default().queue_family_index(queue_family).queue_priorities(&prio)];
        let want_host = extensions.iter().any(|e| e == "VK_EXT_external_memory_host");
        let mut names: Vec<*const c_char> = Vec::new();
        if want_host {
            names.push(ash::ext::external_memory_host::NAME.as_ptr());
        }
        let dci = vk::DeviceCreateInfo::default().queue_create_infos(&qci).enabled_extension_names(&names);
        // SAFETY: estructuras válidas; las extensiones pedidas están en la lista del dispositivo.
        let device = unsafe { instance.create_device(phys, &dci, None) }.map_err(err("vkCreateDevice"))?;
        // SAFETY: familia y cola creadas arriba.
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        let host_memory = want_host.then(|| ash::ext::external_memory_host::Device::new(&instance, &device));
        Ok(Gpu { _entry: entry, instance, phys, device, queue, queue_family, props, mem_props, extensions, host_memory })
    }

    pub fn name(&self) -> String {
        cstr(&self.props.device_name)
    }

    pub fn has_ext(&self, name: &str) -> bool {
        self.extensions.iter().any(|e| e == name)
    }

    /// Un tipo de memoria de `bits` con esas propiedades.
    pub fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> Option<u32> {
        (0..self.mem_props.memory_type_count)
            .find(|&i| bits & (1 << i) != 0 && self.mem_props.memory_types[i as usize].property_flags.contains(flags))
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        // SAFETY: nada usa ya el dispositivo ni la instancia.
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

/// Texto de un arreglo de C terminado en NUL (nombres de Vulkan).
pub fn cstr(b: &[c_char]) -> String {
    let bytes: Vec<u8> = b.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
