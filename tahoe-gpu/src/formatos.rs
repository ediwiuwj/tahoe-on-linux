//! Correspondencias de Metal a Vulkan: formatos de píxel y de vértice, mezcla, topologías y samplers (con los valores
//! numéricos de los enums de Metal).

use ash::vk;

/// MTLPixelFormat → (formato de Vulkan, bytes por píxel). None: sin soporte.
pub fn pixel(f: u32) -> Option<(vk::Format, u32)> {
    use vk::Format as F;
    Some(match f {
        1 => (F::R8_UNORM, 1), // A8Unorm (el alfa en el canal rojo: el shader lo lee como .r)
        10 => (F::R8_UNORM, 1),
        11 => (F::R8_SRGB, 1),
        12 => (F::R8_SNORM, 1),
        13 => (F::R8_UINT, 1),
        14 => (F::R8_SINT, 1),
        20 => (F::R16_UNORM, 2),
        22 => (F::R16_SNORM, 2),
        23 => (F::R16_UINT, 2),
        24 => (F::R16_SINT, 2),
        25 => (F::R16_SFLOAT, 2),
        30 => (F::R8G8_UNORM, 2),
        31 => (F::R8G8_SRGB, 2),
        32 => (F::R8G8_SNORM, 2),
        33 => (F::R8G8_UINT, 2),
        34 => (F::R8G8_SINT, 2),
        40 => (F::B5G6R5_UNORM_PACK16, 2),
        53 => (F::R32_UINT, 4),
        54 => (F::R32_SINT, 4),
        55 => (F::R32_SFLOAT, 4),
        60 => (F::R16G16_UNORM, 4),
        62 => (F::R16G16_SNORM, 4),
        63 => (F::R16G16_UINT, 4),
        64 => (F::R16G16_SINT, 4),
        65 => (F::R16G16_SFLOAT, 4),
        70 => (F::R8G8B8A8_UNORM, 4),
        71 => (F::R8G8B8A8_SRGB, 4),
        72 => (F::R8G8B8A8_SNORM, 4),
        73 => (F::R8G8B8A8_UINT, 4),
        74 => (F::R8G8B8A8_SINT, 4),
        80 => (F::B8G8R8A8_UNORM, 4),
        81 => (F::B8G8R8A8_SRGB, 4),
        90 => (F::A2B10G10R10_UNORM_PACK32, 4),
        91 => (F::A2B10G10R10_UINT_PACK32, 4),
        92 => (F::B10G11R11_UFLOAT_PACK32, 4),
        93 => (F::E5B9G9R9_UFLOAT_PACK32, 4),
        94 => (F::A2R10G10B10_UNORM_PACK32, 4),
        103 => (F::R32G32_UINT, 8),
        104 => (F::R32G32_SINT, 8),
        105 => (F::R32G32_SFLOAT, 8),
        110 => (F::R16G16B16A16_UNORM, 8),
        112 => (F::R16G16B16A16_SNORM, 8),
        113 => (F::R16G16B16A16_UINT, 8),
        114 => (F::R16G16B16A16_SINT, 8),
        115 => (F::R16G16B16A16_SFLOAT, 8),
        123 => (F::R32G32B32A32_UINT, 16),
        124 => (F::R32G32B32A32_SINT, 16),
        125 => (F::R32G32B32A32_SFLOAT, 16),
        250 => (F::D16_UNORM, 2),
        252 => (F::D32_SFLOAT, 4),
        253 => (F::S8_UINT, 1),
        255 => (F::D24_UNORM_S8_UINT, 4),
        260 => (F::D32_SFLOAT_S8_UINT, 8),
        _ => return None,
    })
}

/// MTLVertexFormat → formato de Vulkan.
pub fn vertice(f: u32) -> Option<vk::Format> {
    use vk::Format as F;
    Some(match f {
        1 => F::R8G8_UINT,
        2 => F::R8G8B8_UINT,
        3 => F::R8G8B8A8_UINT,
        4 => F::R8G8_SINT,
        5 => F::R8G8B8_SINT,
        6 => F::R8G8B8A8_SINT,
        7 => F::R8G8_UNORM,
        8 => F::R8G8B8_UNORM,
        9 => F::R8G8B8A8_UNORM,
        10 => F::R8G8_SNORM,
        11 => F::R8G8B8_SNORM,
        12 => F::R8G8B8A8_SNORM,
        13 => F::R16G16_UINT,
        14 => F::R16G16B16_UINT,
        15 => F::R16G16B16A16_UINT,
        16 => F::R16G16_SINT,
        17 => F::R16G16B16_SINT,
        18 => F::R16G16B16A16_SINT,
        19 => F::R16G16_UNORM,
        20 => F::R16G16B16_UNORM,
        21 => F::R16G16B16A16_UNORM,
        22 => F::R16G16_SNORM,
        23 => F::R16G16B16_SNORM,
        24 => F::R16G16B16A16_SNORM,
        25 => F::R16G16_SFLOAT,
        26 => F::R16G16B16_SFLOAT,
        27 => F::R16G16B16A16_SFLOAT,
        28 => F::R32_SFLOAT,
        29 => F::R32G32_SFLOAT,
        30 => F::R32G32B32_SFLOAT,
        31 => F::R32G32B32A32_SFLOAT,
        32 => F::R32_SINT,
        33 => F::R32G32_SINT,
        34 => F::R32G32B32_SINT,
        35 => F::R32G32B32A32_SINT,
        36 => F::R32_UINT,
        37 => F::R32G32_UINT,
        38 => F::R32G32B32_UINT,
        39 => F::R32G32B32A32_UINT,
        40 => F::A2B10G10R10_SNORM_PACK32,
        41 => F::A2B10G10R10_UNORM_PACK32,
        42 => F::B8G8R8A8_UNORM,
        45 => F::R8_UINT,
        46 => F::R8_SINT,
        47 => F::R8_UNORM,
        48 => F::R8_SNORM,
        49 => F::R16_UINT,
        50 => F::R16_SINT,
        51 => F::R16_UNORM,
        52 => F::R16_SNORM,
        53 => F::R16_SFLOAT,
        54 => F::B10G11R11_UFLOAT_PACK32,
        55 => F::E5B9G9R9_UFLOAT_PACK32,
        _ => return None,
    })
}

/// MTLBlendFactor
pub fn factor(f: u32) -> vk::BlendFactor {
    use vk::BlendFactor as B;
    match f {
        0 => B::ZERO,
        1 => B::ONE,
        2 => B::SRC_COLOR,
        3 => B::ONE_MINUS_SRC_COLOR,
        4 => B::SRC_ALPHA,
        5 => B::ONE_MINUS_SRC_ALPHA,
        6 => B::DST_COLOR,
        7 => B::ONE_MINUS_DST_COLOR,
        8 => B::DST_ALPHA,
        9 => B::ONE_MINUS_DST_ALPHA,
        10 => B::SRC_ALPHA_SATURATE,
        11 => B::CONSTANT_COLOR,
        12 => B::ONE_MINUS_CONSTANT_COLOR,
        13 => B::CONSTANT_ALPHA,
        14 => B::ONE_MINUS_CONSTANT_ALPHA,
        15 => B::SRC1_COLOR,
        16 => B::ONE_MINUS_SRC1_COLOR,
        17 => B::SRC1_ALPHA,
        18 => B::ONE_MINUS_SRC1_ALPHA,
        _ => B::ONE,
    }
}

/// MTLBlendOperation
pub fn operacion(o: u32) -> vk::BlendOp {
    match o {
        1 => vk::BlendOp::SUBTRACT,
        2 => vk::BlendOp::REVERSE_SUBTRACT,
        3 => vk::BlendOp::MIN,
        4 => vk::BlendOp::MAX,
        _ => vk::BlendOp::ADD,
    }
}

/// MTLColorWriteMask (rojo 8, verde 4, azul 2, alfa 1)
pub fn mascara(m: u32) -> vk::ColorComponentFlags {
    let mut r = vk::ColorComponentFlags::empty();
    if m & 8 != 0 {
        r |= vk::ColorComponentFlags::R;
    }
    if m & 4 != 0 {
        r |= vk::ColorComponentFlags::G;
    }
    if m & 2 != 0 {
        r |= vk::ColorComponentFlags::B;
    }
    if m & 1 != 0 {
        r |= vk::ColorComponentFlags::A;
    }
    r
}

/// MTLPrimitiveType → topología de Vulkan (punto 0, línea 1, tira de líneas 2, triángulo 3, tira de triángulos 4).
pub fn topologia(p: u32) -> vk::PrimitiveTopology {
    match p {
        0 => vk::PrimitiveTopology::POINT_LIST,
        1 => vk::PrimitiveTopology::LINE_LIST,
        2 => vk::PrimitiveTopology::LINE_STRIP,
        4 => vk::PrimitiveTopology::TRIANGLE_STRIP,
        _ => vk::PrimitiveTopology::TRIANGLE_LIST,
    }
}

/// MTLSamplerAddressMode
pub fn direccion(a: u32) -> vk::SamplerAddressMode {
    match a {
        1 => vk::SamplerAddressMode::MIRROR_CLAMP_TO_EDGE,
        2 => vk::SamplerAddressMode::REPEAT,
        3 => vk::SamplerAddressMode::MIRRORED_REPEAT,
        4 | 5 => vk::SamplerAddressMode::CLAMP_TO_BORDER,
        _ => vk::SamplerAddressMode::CLAMP_TO_EDGE,
    }
}

/// MTLTextureType → forma del motor
pub fn forma(t: u32) -> Option<crate::motor::Forma> {
    use crate::motor::Forma;
    Some(match t {
        0 => Forma::D1,
        1 => Forma::D1Array,
        2 | 4 => Forma::D2,
        3 | 8 => Forma::D2Array,
        5 => Forma::Cubo,
        6 => Forma::CuboArray,
        7 => Forma::D3,
        _ => return None,
    })
}

/// Bytes por texel de un formato de Vulkan (los que salen de `pixel`).
pub fn bytes_de(f: vk::Format) -> Option<u32> {
    (1..=600).find_map(|m| pixel(m).filter(|(g, _)| *g == f).map(|(_, b)| b))
}

/// Aspecto de un formato (color, profundidad o estarcido).
pub fn aspecto(f: vk::Format) -> vk::ImageAspectFlags {
    match f {
        vk::Format::D16_UNORM | vk::Format::D32_SFLOAT => vk::ImageAspectFlags::DEPTH,
        vk::Format::S8_UINT => vk::ImageAspectFlags::STENCIL,
        vk::Format::D24_UNORM_S8_UINT | vk::Format::D32_SFLOAT_S8_UINT => vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
        _ => vk::ImageAspectFlags::COLOR,
    }
}
