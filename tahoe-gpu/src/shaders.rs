//! SPIR-V generado con rspirv (el mismo camino que seguirá el traductor de shaders de Metal).

use rspirv::binary::Assemble;
use rspirv::dr::{Builder, Operand};
use rspirv::spirv::{
    AddressingModel, BuiltIn, Capability, Decoration, ExecutionMode, ExecutionModel, FunctionControl, MemoryModel,
    StorageClass,
};

/// Kernel de prueba, como este de Metal:
/// `kernel void k(device uint* d [[buffer(0)]], uint i [[thread_position_in_grid]]) { d[i] = d[i] * 2 + 1; }`
/// (grupos de 64 hilos; el buffer va en el binding 0 del conjunto 0).
pub fn doble_mas_uno() -> Vec<u32> {
    let mut b = Builder::new();
    b.set_version(1, 3);
    b.capability(Capability::Shader);
    b.memory_model(AddressingModel::Logical, MemoryModel::GLSL450);
    let void = b.type_void();
    let u32t = b.type_int(32, 0);
    let v3 = b.type_vector(u32t, 3);
    let rta = b.type_runtime_array(u32t);
    b.decorate(rta, Decoration::ArrayStride, [Operand::LiteralBit32(4)]);
    let st = b.type_struct([rta]);
    b.decorate(st, Decoration::Block, []);
    b.member_decorate(st, 0, Decoration::Offset, [Operand::LiteralBit32(0)]);
    let p_st = b.type_pointer(None, StorageClass::StorageBuffer, st);
    let p_u32 = b.type_pointer(None, StorageClass::StorageBuffer, u32t);
    let p_v3 = b.type_pointer(None, StorageClass::Input, v3);
    let buf = b.variable(p_st, None, StorageClass::StorageBuffer, None);
    b.decorate(buf, Decoration::DescriptorSet, [Operand::LiteralBit32(0)]);
    b.decorate(buf, Decoration::Binding, [Operand::LiteralBit32(0)]);
    let gid = b.variable(p_v3, None, StorageClass::Input, None);
    b.decorate(gid, Decoration::BuiltIn, [Operand::BuiltIn(BuiltIn::GlobalInvocationId)]);
    let c0 = b.constant_bit32(u32t, 0);
    let c1 = b.constant_bit32(u32t, 1);
    let c2 = b.constant_bit32(u32t, 2);
    let fty = b.type_function(void, []);
    let main = b.begin_function(void, None, FunctionControl::NONE, fty).unwrap();
    b.begin_block(None).unwrap();
    let g = b.load(v3, None, gid, None, []).unwrap();
    let x = b.composite_extract(u32t, None, g, [0]).unwrap();
    let p = b.access_chain(p_u32, None, buf, [c0, x]).unwrap();
    let v = b.load(u32t, None, p, None, []).unwrap();
    let v2 = b.i_mul(u32t, None, v, c2).unwrap();
    let v3_ = b.i_add(u32t, None, v2, c1).unwrap();
    b.store(p, v3_, None, []).unwrap();
    b.ret().unwrap();
    b.end_function().unwrap();
    b.entry_point(ExecutionModel::GLCompute, main, "main", [gid]);
    b.execution_mode(main, ExecutionMode::LocalSize, [64, 1, 1]);
    b.module().assemble()
}

#[cfg(test)]
mod tests {
    #[test]
    fn spirv_valido_en_apariencia() {
        let w = super::doble_mas_uno();
        assert_eq!(w[0], 0x0723_0203); // número mágico de SPIR-V
        assert_eq!(w[1], 0x0001_0300); // versión 1.3
        assert!(w.len() > 50);
    }
}
