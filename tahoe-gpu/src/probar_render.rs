//! `tahoe-gpu probar-render <metallib de QuartzCore>`: prueba del render del motor con shaders de macOS. std_vert0_lpf
//! (posición = matriz × posición, atributos 0, 1 y 3) y attachment_clear_frag_lpf (escribe 0): se limpia un BGRA8 de
//! 64×64 a rojo y se dibuja el triángulo (-1,-1) (1,-1) (-1,1), que en Metal (Y hacia arriba) cubre la mitad de abajo a
//! la izquierda. Comprueba pipeline, atributos, buffers por etapa y la orientación.

use std::collections::HashMap;

use ash::vk;

use crate::air::{self, spirv};
use crate::metallib;
use crate::motor::{Enlazado, Motor};
use crate::render::{Atributo, Carga, ColorPase, DescRender, Dibujo, EnlaceVertices, Mezcla};
use crate::vk::Res;

fn traducir(b: &metallib::Biblioteca, nombre: &str, op: &spirv::Opciones) -> Res<spirv::Traduccion> {
    let f = b.funciones.iter().find(|f| f.nombre == nombre).ok_or_else(|| format!("no está {nombre}"))?;
    let m = air::ir::leer(b.bitcode(f)?)?;
    let e = air::entradas(&m).into_iter().next().ok_or("sin entrada")?;
    spirv::traducir(&m, &e, op)
}

pub fn probar_render(a: Vec<String>) -> Res<()> {
    let ruta = a.first().ok_or("uso: tahoe-gpu probar-render <default.metallib de QuartzCore>")?;
    let d = std::fs::read(ruta).map_err(|e| format!("{ruta}: {e}"))?;
    let b = metallib::Biblioteca::leer(&d)?;
    let v = traducir(&b, "std_vert0_lpf", &spirv::Opciones { set: 0, ..Default::default() })?;
    let varyings: HashMap<String, u32> = v.varyings.iter().cloned().collect();
    let f = traducir(&b, "attachment_clear_frag_lpf", &spirv::Opciones { set: 1, varyings, ..Default::default() })?;
    println!("vertex: atributos {:?}, enlaces {:?}; fragment: colores {:?}", v.atributos, v.enlaces.iter().map(|e| e.binding).collect::<Vec<_>>(), f.colores);
    let m = Motor::new()?;
    let fmt = vk::Format::B8G8R8A8_UNORM;
    let atributos: Vec<Atributo> = v
        .atributos
        .iter()
        .map(|&(attr, _, ref t)| Atributo {
            location: attr,
            buffer: 0,
            formato: if t.contains("2 x") { vk::Format::R32G32_SFLOAT } else { vk::Format::R32G32B32A32_SFLOAT },
            offset: if attr == 3 { 16 } else { 0 },
        })
        .collect();
    let p = m.pipeline_render(&DescRender {
        vertex: &v,
        fragment: Some(&f),
        atributos,
        enlaces_vertices: vec![EnlaceVertices { buffer: 0, paso: 32, por_instancia: false }],
        colores: f.colores.iter().map(|_| (fmt, Mezcla::default())).collect(),
        topologia: vk::PrimitiveTopology::TRIANGLE_LIST,
    })?;
    // vértices: posición (float4) y color (float4)
    let vb = m.buffer(4096)?;
    let vs: [[f32; 8]; 3] = [[-1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0], [1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0], [-1.0, 1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 1.0]];
    for (i, x) in vs.iter().flatten().enumerate() {
        vb.bytes()[i * 4..i * 4 + 4].copy_from_slice(&x.to_le_bytes());
    }
    // matriz identidad en el buffer 2 del vertex shader
    let mb = m.buffer(4096)?;
    for i in 0..4 {
        mb.bytes()[(i * 4 + i) * 4..(i * 4 + i) * 4 + 4].copy_from_slice(&1.0f32.to_le_bytes());
    }
    let otro = m.buffer(4096)?;
    let mut rv: HashMap<u32, Enlazado> = HashMap::new();
    for e in &v.enlaces {
        rv.insert(e.binding, Enlazado::Buffer(if e.binding == 2 { &mb } else { &otro }));
    }
    let n_colores = f.colores.len().max(1);
    let texturas: Vec<_> = (0..n_colores).map(|_| m.textura(64, 64, fmt)).collect::<Res<_>>()?;
    let colores: Vec<ColorPase> = texturas.iter().map(|t| ColorPase { textura: t, nivel: 0, capa: 0, carga: Carga::Limpiar([1.0, 0.0, 0.0, 1.0]), guardar: true }).collect();
    let dib = Dibujo {
        pipeline: &p,
        recursos: [rv, HashMap::new()],
        vertices: vec![(0, &vb, 0)],
        indices: None,
        primero: 0,
        cuenta: 3,
        instancias: 1,
        primera_instancia: 0,
        base_vertice: 0,
        viewport: [0.0, 0.0, 64.0, 64.0, 0.0, 1.0],
        tijera: None,
        cull: vk::CullModeFlags::NONE,
        horario: false,
    };
    m.pase_render(&colores, &[dib])?;
    let down = m.buffer(64 * 64 * 4)?;
    m.copiar(&texturas[0], &down, false)?;
    let px = |x: usize, y: usize| -> [u8; 4] { down.bytes()[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4].try_into().unwrap() };
    let (abajo_izq, arriba_der) = (px(8, 56), px(56, 8));
    println!("píxel abajo a la izquierda (8,56) BGRA = {abajo_izq:?}; arriba a la derecha (56,8) = {arriba_der:?}");
    let bien = abajo_izq == [0, 0, 0, 0] && arriba_der == [0, 0, 255, 255];
    println!("render: {}", if bien { "CORRECTO" } else { "MAL" });
    if bien {
        Ok(())
    } else {
        Err("el render no da lo esperado".into())
    }
}
