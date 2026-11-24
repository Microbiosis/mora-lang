//! Pure-Rust SPIR-V 1.0 `GLCompute` shader emitters for Vulkan 1.0 hardware GPU dispatch.
//!
//! Zero external SDK (`shaderc` / `glslang`) or Cargo dependencies, and zero optional Vulkan
//! extensions required (pure SPIR-V 1.0 + `GLSL.std.450`, portable across NVIDIA, AMD, Intel,
//! and Apple MoltenVK):
//! 1-3.  `emit_spirv_fused_linear_f32` / `emit_spirv_fused_linear_half(false|true)`:
//!       2D & broadcast batched ND MatMul + Linear + Bias + Activation (`f32`/`f16`/`bf16`)
//! 4-6.  `emit_spirv_rms_norm_f32` / `emit_spirv_rms_norm_half(false|true)`:
//!       GPU row-wise RMSNorm [* gamma] for `FusedRmsNormLinear` (`f32`/`f16`/`bf16`)
//! 7-9.  `emit_spirv_flash_attention_f32` / `emit_spirv_flash_attention_half(false|true)`:
//!       single-pass online-softmax FlashAttention-2 with GQA/mask (`f32`/`f16`/`bf16`)
//! 10-12. `emit_spirv_fused_elementwise_f32` / `emit_spirv_fused_elementwise_half(false|true)`:
//!       single-launch 25-opcode bytecode broadcast evaluator (`f32`/`f16`/`bf16`)

const OP_EXT_INST_IMPORT: u16 = 11;
const OP_EXT_INST: u16 = 12;
const OP_MEMORY_MODEL: u16 = 14;
const OP_ENTRY_POINT: u16 = 15;
const OP_EXECUTION_MODE: u16 = 16;
const OP_CAPABILITY: u16 = 17;
const OP_TYPE_VOID: u16 = 19;
const OP_TYPE_BOOL: u16 = 20;
const OP_TYPE_INT: u16 = 21;
const OP_TYPE_FLOAT: u16 = 22;
const OP_TYPE_VECTOR: u16 = 23;
const OP_TYPE_ARRAY: u16 = 28;
const OP_TYPE_RUNTIME_ARRAY: u16 = 29;
const OP_TYPE_STRUCT: u16 = 30;
const OP_TYPE_POINTER: u16 = 32;
const OP_TYPE_FUNCTION: u16 = 33;
const OP_CONSTANT: u16 = 43;
const OP_FUNCTION: u16 = 54;
const OP_FUNCTION_END: u16 = 56;
const OP_VARIABLE: u16 = 59;
const OP_LOAD: u16 = 61;
const OP_STORE: u16 = 62;
const OP_ACCESS_CHAIN: u16 = 65;
const OP_DECORATE: u16 = 71;
const OP_MEMBER_DECORATE: u16 = 72;
const OP_COMPOSITE_CONSTRUCT: u16 = 80;
const OP_COMPOSITE_EXTRACT: u16 = 81;
const OP_CONVERT_U_TO_F: u16 = 112;
const OP_BITCAST: u16 = 124;
const OP_FNEGATE: u16 = 127;
const OP_IADD: u16 = 128;
const OP_FADD: u16 = 129;
const OP_ISUB: u16 = 130;
const OP_FSUB: u16 = 131;
const OP_IMUL: u16 = 132;
const OP_FMUL: u16 = 133;
const OP_UDIV: u16 = 134;
const OP_FDIV: u16 = 136;
const OP_UMOD: u16 = 137;
const OP_LOGICAL_AND: u16 = 167;
const OP_SELECT: u16 = 169;
const OP_IEQUAL: u16 = 170;
const OP_INOT_EQUAL: u16 = 171;
const OP_UGREATER_THAN: u16 = 172;
const OP_ULESS_THAN: u16 = 176;
const OP_FORD_EQUAL: u16 = 180;
const OP_FORD_GREATER_THAN: u16 = 186;
const OP_SHIFT_RIGHT_LOGICAL: u16 = 194;
const OP_SHIFT_LEFT_LOGICAL: u16 = 196;
const OP_BITWISE_OR: u16 = 197;
const OP_BITWISE_AND: u16 = 199;
const OP_PHI: u16 = 245;
const OP_LOOP_MERGE: u16 = 246;
const OP_SELECTION_MERGE: u16 = 247;
const OP_LABEL: u16 = 248;
const OP_BRANCH: u16 = 249;
const OP_BRANCH_CONDITIONAL: u16 = 250;
const OP_SWITCH: u16 = 251;
const OP_RETURN: u16 = 253;

const GLSL_FABS: u32 = 4;
const GLSL_SIN: u32 = 13;
const GLSL_COS: u32 = 14;
const GLSL_TANH: u32 = 21;
const GLSL_POW: u32 = 26;
const GLSL_EXP: u32 = 27;
const GLSL_LOG: u32 = 28;
const GLSL_SQRT: u32 = 31;
const GLSL_FMAX: u32 = 40;
const GLSL_FCLAMP: u32 = 43;
const GLSL_PACK_HALF_2X16: u32 = 58;
const GLSL_UNPACK_HALF_2X16: u32 = 62;

const SC_INPUT: u32 = 1;
const SC_UNIFORM: u32 = 2;
const SC_FUNCTION: u32 = 7;

const DEC_BUFFER_BLOCK: u32 = 3;
const DEC_ARRAY_STRIDE: u32 = 6;
const DEC_BUILT_IN: u32 = 11;
const DEC_BINDING: u32 = 33;
const DEC_DESCRIPTOR_SET: u32 = 34;
const DEC_OFFSET: u32 = 35;

pub(super) struct SpirvBuilder {
    words: Vec<u32>,
    next_id: u32,
}

impl SpirvBuilder {
    fn new() -> Self {
        Self {
            words: vec![
                0x0723_0203, // Magic
                0x0001_0000, // SPIR-V 1.0
                0x0000_0000, // Generator
                0,           // Bound (patched on finish)
                0,           // Schema
            ],
            next_id: 1,
        }
    }

    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn inst(&mut self, opcode: u16, operands: &[u32]) {
        let word_count = (1 + operands.len()) as u32;
        self.words.push((word_count << 16) | u32::from(opcode));
        self.words.extend_from_slice(operands);
    }

    fn push_str_words(dst: &mut Vec<u32>, s: &str) {
        let bytes = s.as_bytes();
        let total = bytes.len() + 1;
        let n_words = total.div_ceil(4);
        for w in 0..n_words {
            let mut word = 0u32;
            for b in 0..4 {
                let idx = w * 4 + b;
                let byte = if idx < bytes.len() { bytes[idx] } else { 0 };
                word |= u32::from(byte) << (b * 8);
            }
            dst.push(word);
        }
    }

    fn c_u32(&mut self, ty_u32: u32, v: u32) -> u32 {
        let id = self.alloc_id();
        self.inst(OP_CONSTANT, &[ty_u32, id, v]);
        id
    }

    fn c_f32(&mut self, ty_f32: u32, v: f32) -> u32 {
        let id = self.alloc_id();
        self.inst(OP_CONSTANT, &[ty_f32, id, v.to_bits()]);
        id
    }

    fn load_buf(&mut self, ptr_ty: u32, elem_ty: u32, var_buf: u32, c_u0: u32, idx: u32) -> u32 {
        let p = self.alloc_id();
        self.inst(OP_ACCESS_CHAIN, &[ptr_ty, p, var_buf, c_u0, idx]);
        let v = self.alloc_id();
        self.inst(OP_LOAD, &[elem_ty, v, p]);
        v
    }

    fn store_buf(&mut self, ptr_ty: u32, var_buf: u32, c_u0: u32, idx: u32, val: u32) {
        let p = self.alloc_id();
        self.inst(OP_ACCESS_CHAIN, &[ptr_ty, p, var_buf, c_u0, idx]);
        self.inst(OP_STORE, &[p, val]);
    }

    fn finish(mut self) -> Vec<u32> {
        self.words[3] = self.next_id;
        self.words
    }
}

struct ActConsts {
    c_u1: u32,
    c_u2: u32,
    c_u3: u32,
    c_u4: u32,
    c_u5: u32,
    c_f0: u32,
    c_f1: u32,
    c_f05: u32,
    c_gelu_a: u32,
    c_gelu_b: u32,
    c_f_lo: u32,
    c_f_hi: u32,
}

#[allow(clippy::too_many_arguments)]
fn emit_apply_act(
    b: &mut SpirvBuilder,
    glsl_ext: u32,
    ty_f32: u32,
    ty_bool: u32,
    c: &ActConsts,
    x: u32,
    p_act: u32,
) -> u32 {
    let v_relu = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, v_relu, glsl_ext, GLSL_FMAX, x, c.c_f0],
    );

    let neg_x = b.alloc_id();
    b.inst(OP_FNEGATE, &[ty_f32, neg_x, x]);
    let neg_x_c = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[
            ty_f32,
            neg_x_c,
            glsl_ext,
            GLSL_FCLAMP,
            neg_x,
            c.c_f_lo,
            c.c_f_hi,
        ],
    );
    let exp_neg_x = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, exp_neg_x, glsl_ext, GLSL_EXP, neg_x_c],
    );
    let one_p_exp = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, one_p_exp, c.c_f1, exp_neg_x]);
    let v_sig = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, v_sig, c.c_f1, one_p_exp]);
    let v_silu = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, v_silu, x, v_sig]);

    let v_tanh = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, v_tanh, glsl_ext, GLSL_TANH, x]);

    let x2 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, x2, x, x]);
    let x3 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, x3, x2, x]);
    let cx3 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, cx3, c.c_gelu_b, x3]);
    let inner1 = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, inner1, x, cx3]);
    let inner2 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, inner2, c.c_gelu_a, inner1]);
    let th = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, th, glsl_ext, GLSL_TANH, inner2]);
    let one_p_th = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, one_p_th, c.c_f1, th]);
    let half_x = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, half_x, c.c_f05, x]);
    let v_gelu = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, v_gelu, half_x, one_p_th]);

    let mut cur = x;
    for (code_c, act_val) in [
        (c.c_u1, v_relu),
        (c.c_u2, v_gelu),
        (c.c_u3, v_silu),
        (c.c_u4, v_tanh),
        (c.c_u5, v_sig),
    ] {
        let eq = b.alloc_id();
        b.inst(OP_IEQUAL, &[ty_bool, eq, p_act, code_c]);
        let next = b.alloc_id();
        b.inst(OP_SELECT, &[ty_f32, next, eq, act_val, cur]);
        cur = next;
    }
    cur
}

/// Shader 1 (7 SSBOs): Batched/2D `FusedLinearAct` & `MatMul` in `f32`.
/// Bindings: `0=A(f32[])`, `1=B(f32[])`, `2=Bias(f32[])`, `3=Out(f32[])`,
/// `4=Params(u32[])` (`[M, N, K, has_bias, bias_len, act_code, batches]`),
/// `5=ABases(u32[])`, `6=BBases(u32[])`.
pub(super) fn emit_spirv_fused_linear_f32() -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    b.inst(OP_CAPABILITY, &[1]); // Shader
    let glsl_ext = b.alloc_id();
    let mut ext_ops = vec![glsl_ext];
    SpirvBuilder::push_str_words(&mut ext_ops, "GLSL.std.450");
    b.inst(OP_EXT_INST_IMPORT, &ext_ops);
    b.inst(OP_MEMORY_MODEL, &[0, 1]);

    let main_fn = b.alloc_id();
    let var_gid = b.alloc_id();
    let mut ep_ops = vec![5, main_fn];
    SpirvBuilder::push_str_words(&mut ep_ops, "main");
    ep_ops.push(var_gid);
    b.inst(OP_ENTRY_POINT, &ep_ops);
    b.inst(OP_EXECUTION_MODE, &[main_fn, 17, 16, 16, 1]);

    let ty_f32_arr = b.alloc_id();
    let ty_f32_buf = b.alloc_id();
    let ty_u32_arr = b.alloc_id();
    let ty_u32_buf = b.alloc_id();
    let var_a = b.alloc_id();
    let var_b = b.alloc_id();
    let var_bias = b.alloc_id();
    let var_out = b.alloc_id();
    let var_params = b.alloc_id();
    let var_ab = b.alloc_id();
    let var_bb = b.alloc_id();

    b.inst(OP_DECORATE, &[var_gid, DEC_BUILT_IN, 28]);
    b.inst(OP_DECORATE, &[ty_f32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_f32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_f32_buf, 0, DEC_OFFSET, 0]);
    b.inst(OP_DECORATE, &[ty_u32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_u32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_u32_buf, 0, DEC_OFFSET, 0]);
    for (binding, var_id) in [
        (0u32, var_a),
        (1, var_b),
        (2, var_bias),
        (3, var_out),
        (4, var_params),
        (5, var_ab),
        (6, var_bb),
    ] {
        b.inst(OP_DECORATE, &[var_id, DEC_DESCRIPTOR_SET, 0]);
        b.inst(OP_DECORATE, &[var_id, DEC_BINDING, binding]);
    }

    let ty_void = b.alloc_id();
    b.inst(OP_TYPE_VOID, &[ty_void]);
    let ty_fn = b.alloc_id();
    b.inst(OP_TYPE_FUNCTION, &[ty_fn, ty_void]);
    let ty_bool = b.alloc_id();
    b.inst(OP_TYPE_BOOL, &[ty_bool]);
    let ty_u32 = b.alloc_id();
    b.inst(OP_TYPE_INT, &[ty_u32, 32, 0]);
    let ty_f32 = b.alloc_id();
    b.inst(OP_TYPE_FLOAT, &[ty_f32, 32]);
    let ty_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_uvec3, ty_u32, 3]);
    let ptr_in_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_in_uvec3, SC_INPUT, ty_uvec3]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_f32_arr, ty_f32]);
    b.inst(OP_TYPE_STRUCT, &[ty_f32_buf, ty_f32_arr]);
    let ptr_unif_f32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_f32_buf, SC_UNIFORM, ty_f32_buf]);
    let ptr_unif_f32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_f32, SC_UNIFORM, ty_f32]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_u32_arr, ty_u32]);
    b.inst(OP_TYPE_STRUCT, &[ty_u32_buf, ty_u32_arr]);
    let ptr_unif_u32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32_buf, SC_UNIFORM, ty_u32_buf]);
    let ptr_unif_u32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32, SC_UNIFORM, ty_u32]);

    let c_u0 = b.c_u32(ty_u32, 0);
    let c_u1 = b.c_u32(ty_u32, 1);
    let c_u2 = b.c_u32(ty_u32, 2);
    let c_u3 = b.c_u32(ty_u32, 3);
    let c_u4 = b.c_u32(ty_u32, 4);
    let c_u5 = b.c_u32(ty_u32, 5);
    let c_u6 = b.c_u32(ty_u32, 6);
    let act_c = ActConsts {
        c_u1,
        c_u2,
        c_u3,
        c_u4,
        c_u5,
        c_f0: b.c_f32(ty_f32, 0.0),
        c_f1: b.c_f32(ty_f32, 1.0),
        c_f05: b.c_f32(ty_f32, 0.5),
        c_gelu_a: b.c_f32(ty_f32, 0.797_884_6),
        c_gelu_b: b.c_f32(ty_f32, 0.044_715),
        c_f_lo: b.c_f32(ty_f32, -40.0),
        c_f_hi: b.c_f32(ty_f32, 40.0),
    };

    b.inst(OP_VARIABLE, &[ptr_in_uvec3, var_gid, SC_INPUT]);
    b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, var_a, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, var_b, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, var_bias, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, var_out, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, var_params, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, var_ab, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, var_bb, SC_UNIFORM]);

    b.inst(OP_FUNCTION, &[ty_void, main_fn, 0, ty_fn]);
    let lbl_entry = b.alloc_id();
    let lbl_body = b.alloc_id();
    let lbl_loop_hdr = b.alloc_id();
    let lbl_loop_body = b.alloc_id();
    let lbl_loop_cont = b.alloc_id();
    let lbl_loop_exit = b.alloc_id();
    let lbl_exit = b.alloc_id();

    b.inst(OP_LABEL, &[lbl_entry]);
    let gid_val = b.alloc_id();
    b.inst(OP_LOAD, &[ty_uvec3, gid_val, var_gid]);
    let col = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, col, gid_val, 0]);
    let row = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, row, gid_val, 1]);
    let batch = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, batch, gid_val, 2]);

    let p_m = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u0);
    let p_n = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u1);
    let p_k = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u2);
    let p_has_bias = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u3);
    let p_bias_len = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u4);
    let p_act = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u5);
    let p_batches = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u6);

    let cmp_row = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, cmp_row, row, p_m]);
    let cmp_col = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, cmp_col, col, p_n]);
    let cmp_batch = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, cmp_batch, batch, p_batches]);
    let ib0 = b.alloc_id();
    b.inst(OP_LOGICAL_AND, &[ty_bool, ib0, cmp_row, cmp_col]);
    let in_bounds = b.alloc_id();
    b.inst(OP_LOGICAL_AND, &[ty_bool, in_bounds, ib0, cmp_batch]);
    b.inst(OP_SELECTION_MERGE, &[lbl_exit, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[in_bounds, lbl_body, lbl_exit]);

    b.inst(OP_LABEL, &[lbl_body]);
    let a_batch_base = b.load_buf(ptr_unif_u32, ty_u32, var_ab, c_u0, batch);
    let b_batch_base = b.load_buf(ptr_unif_u32, ty_u32, var_bb, c_u0, batch);
    let row_k = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, row_k, row, p_k]);
    let a_row_base = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, a_row_base, a_batch_base, row_k]);
    b.inst(OP_BRANCH, &[lbl_loop_hdr]);

    let t_phi = b.alloc_id();
    let acc_phi = b.alloc_id();
    let t_next = b.alloc_id();
    let acc_next = b.alloc_id();

    b.inst(OP_LABEL, &[lbl_loop_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, t_phi, c_u0, lbl_body, t_next, lbl_loop_cont],
    );
    b.inst(
        OP_PHI,
        &[
            ty_f32,
            acc_phi,
            act_c.c_f0,
            lbl_body,
            acc_next,
            lbl_loop_cont,
        ],
    );
    let loop_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, loop_cond, t_phi, p_k]);
    b.inst(OP_LOOP_MERGE, &[lbl_loop_exit, lbl_loop_cont, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[loop_cond, lbl_loop_body, lbl_loop_exit],
    );

    b.inst(OP_LABEL, &[lbl_loop_body]);
    let a_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, a_idx, a_row_base, t_phi]);
    let a_val = b.load_buf(ptr_unif_f32, ty_f32, var_a, c_u0, a_idx);

    let tn = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, tn, t_phi, p_n]);
    let b_idx0 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, b_idx0, b_batch_base, tn]);
    let b_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, b_idx, b_idx0, col]);
    let b_val = b.load_buf(ptr_unif_f32, ty_f32, var_b, c_u0, b_idx);

    let prod = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, prod, a_val, b_val]);
    b.inst(OP_FADD, &[ty_f32, acc_next, acc_phi, prod]);
    b.inst(OP_BRANCH, &[lbl_loop_cont]);

    b.inst(OP_LABEL, &[lbl_loop_cont]);
    b.inst(OP_IADD, &[ty_u32, t_next, t_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_loop_hdr]);

    b.inst(OP_LABEL, &[lbl_loop_exit]);
    let bias_idx = b.alloc_id();
    b.inst(OP_UMOD, &[ty_u32, bias_idx, col, p_bias_len]);
    let bias_val = b.load_buf(ptr_unif_f32, ty_f32, var_bias, c_u0, bias_idx);
    let acc_plus_bias = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, acc_plus_bias, acc_phi, bias_val]);
    let has_bias_bool = b.alloc_id();
    b.inst(OP_INOT_EQUAL, &[ty_bool, has_bias_bool, p_has_bias, c_u0]);
    let x = b.alloc_id();
    b.inst(
        OP_SELECT,
        &[ty_f32, x, has_bias_bool, acc_plus_bias, acc_phi],
    );

    let out_val = emit_apply_act(&mut b, glsl_ext, ty_f32, ty_bool, &act_c, x, p_act);

    let mn = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, mn, p_m, p_n]);
    let b_mn = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, b_mn, batch, mn]);
    let row_n = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, row_n, row, p_n]);
    let out_i0 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, out_i0, b_mn, row_n]);
    let out_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, out_idx, out_i0, col]);
    b.store_buf(ptr_unif_f32, var_out, c_u0, out_idx, out_val);
    b.inst(OP_BRANCH, &[lbl_exit]);

    b.inst(OP_LABEL, &[lbl_exit]);
    b.inst(OP_RETURN, &[]);
    b.inst(OP_FUNCTION_END, &[]);
    b.finish()
}

/// Shaders 2 & 3 (7 SSBOs, all `u32[]`): Native race-free packed 16-bit (`f16` or `bf16`)
/// Batched/2D MatMul + FusedLinearAct computing 2 adjacent output elements per thread.
pub(super) fn emit_spirv_fused_linear_half(is_bf16: bool) -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    b.inst(OP_CAPABILITY, &[1]); // Shader
    let glsl_ext = b.alloc_id();
    let mut ext_ops = vec![glsl_ext];
    SpirvBuilder::push_str_words(&mut ext_ops, "GLSL.std.450");
    b.inst(OP_EXT_INST_IMPORT, &ext_ops);
    b.inst(OP_MEMORY_MODEL, &[0, 1]);

    let main_fn = b.alloc_id();
    let var_gid = b.alloc_id();
    let mut ep_ops = vec![5, main_fn];
    SpirvBuilder::push_str_words(&mut ep_ops, "main");
    ep_ops.push(var_gid);
    b.inst(OP_ENTRY_POINT, &ep_ops);
    b.inst(OP_EXECUTION_MODE, &[main_fn, 17, 64, 1, 1]);

    let ty_u32_arr = b.alloc_id();
    let ty_u32_buf = b.alloc_id();
    let var_a = b.alloc_id();
    let var_b = b.alloc_id();
    let var_bias = b.alloc_id();
    let var_out = b.alloc_id();
    let var_params = b.alloc_id();
    let var_ab = b.alloc_id();
    let var_bb = b.alloc_id();

    b.inst(OP_DECORATE, &[var_gid, DEC_BUILT_IN, 28]);
    b.inst(OP_DECORATE, &[ty_u32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_u32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_u32_buf, 0, DEC_OFFSET, 0]);
    for (binding, var_id) in [
        (0u32, var_a),
        (1, var_b),
        (2, var_bias),
        (3, var_out),
        (4, var_params),
        (5, var_ab),
        (6, var_bb),
    ] {
        b.inst(OP_DECORATE, &[var_id, DEC_DESCRIPTOR_SET, 0]);
        b.inst(OP_DECORATE, &[var_id, DEC_BINDING, binding]);
    }

    let ty_void = b.alloc_id();
    b.inst(OP_TYPE_VOID, &[ty_void]);
    let ty_fn = b.alloc_id();
    b.inst(OP_TYPE_FUNCTION, &[ty_fn, ty_void]);
    let ty_bool = b.alloc_id();
    b.inst(OP_TYPE_BOOL, &[ty_bool]);
    let ty_u32 = b.alloc_id();
    b.inst(OP_TYPE_INT, &[ty_u32, 32, 0]);
    let ty_f32 = b.alloc_id();
    b.inst(OP_TYPE_FLOAT, &[ty_f32, 32]);
    let ty_vec2 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_vec2, ty_f32, 2]);
    let ty_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_uvec3, ty_u32, 3]);
    let ptr_in_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_in_uvec3, SC_INPUT, ty_uvec3]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_u32_arr, ty_u32]);
    b.inst(OP_TYPE_STRUCT, &[ty_u32_buf, ty_u32_arr]);
    let ptr_unif_u32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32_buf, SC_UNIFORM, ty_u32_buf]);
    let ptr_unif_u32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32, SC_UNIFORM, ty_u32]);

    let c_u0 = b.c_u32(ty_u32, 0);
    let c_u1 = b.c_u32(ty_u32, 1);
    let c_u2 = b.c_u32(ty_u32, 2);
    let c_u3 = b.c_u32(ty_u32, 3);
    let c_u4 = b.c_u32(ty_u32, 4);
    let c_u5 = b.c_u32(ty_u32, 5);
    let c_u6 = b.c_u32(ty_u32, 6);
    let c_u16 = b.c_u32(ty_u32, 16);
    let c_mask_hi = b.c_u32(ty_u32, 0xffff_0000);
    let c_mask_lo = b.c_u32(ty_u32, 0x0000_ffff);
    let c_mask_abs = b.c_u32(ty_u32, 0x7fff_ffff);
    let c_inf = b.c_u32(ty_u32, 0x7f80_0000);
    let c_round_bias = b.c_u32(ty_u32, 0x0000_7fff);
    let c_qnan = b.c_u32(ty_u32, 0x0000_0040);

    let ty_arr2_f32 = b.alloc_id();
    b.inst(OP_TYPE_ARRAY, &[ty_arr2_f32, ty_f32, c_u2]);
    let ptr_fn_arr2 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_arr2, SC_FUNCTION, ty_arr2_f32]);
    let ptr_fn_f32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_f32, SC_FUNCTION, ty_f32]);

    let act_c = ActConsts {
        c_u1,
        c_u2,
        c_u3,
        c_u4,
        c_u5,
        c_f0: b.c_f32(ty_f32, 0.0),
        c_f1: b.c_f32(ty_f32, 1.0),
        c_f05: b.c_f32(ty_f32, 0.5),
        c_gelu_a: b.c_f32(ty_f32, 0.797_884_6),
        c_gelu_b: b.c_f32(ty_f32, 0.044_715),
        c_f_lo: b.c_f32(ty_f32, -40.0),
        c_f_hi: b.c_f32(ty_f32, 40.0),
    };

    b.inst(OP_VARIABLE, &[ptr_in_uvec3, var_gid, SC_INPUT]);
    b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, var_a, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, var_b, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, var_bias, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, var_out, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, var_params, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, var_ab, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, var_bb, SC_UNIFORM]);

    // Helper: unpack element `idx` from a packed-u32 16-bit buffer into f32
    let unpack_half = |b: &mut SpirvBuilder, var_buf: u32, idx: u32| -> u32 {
        let word_idx = b.alloc_id();
        b.inst(OP_SHIFT_RIGHT_LOGICAL, &[ty_u32, word_idx, idx, c_u1]);
        let word = b.load_buf(ptr_unif_u32, ty_u32, var_buf, c_u0, word_idx);
        let lane_bit = b.alloc_id();
        b.inst(OP_BITWISE_AND, &[ty_u32, lane_bit, idx, c_u1]);
        let is_odd = b.alloc_id();
        b.inst(OP_INOT_EQUAL, &[ty_bool, is_odd, lane_bit, c_u0]);
        if is_bf16 {
            let lo_bits = b.alloc_id();
            b.inst(OP_SHIFT_LEFT_LOGICAL, &[ty_u32, lo_bits, word, c_u16]);
            let hi_bits = b.alloc_id();
            b.inst(OP_BITWISE_AND, &[ty_u32, hi_bits, word, c_mask_hi]);
            let bits = b.alloc_id();
            b.inst(OP_SELECT, &[ty_u32, bits, is_odd, hi_bits, lo_bits]);
            let fval = b.alloc_id();
            b.inst(OP_BITCAST, &[ty_f32, fval, bits]);
            fval
        } else {
            let pair = b.alloc_id();
            b.inst(
                OP_EXT_INST,
                &[ty_vec2, pair, glsl_ext, GLSL_UNPACK_HALF_2X16, word],
            );
            let f0 = b.alloc_id();
            b.inst(OP_COMPOSITE_EXTRACT, &[ty_f32, f0, pair, 0]);
            let f1 = b.alloc_id();
            b.inst(OP_COMPOSITE_EXTRACT, &[ty_f32, f1, pair, 1]);
            let fval = b.alloc_id();
            b.inst(OP_SELECT, &[ty_f32, fval, is_odd, f1, f0]);
            fval
        }
    };

    b.inst(OP_FUNCTION, &[ty_void, main_fn, 0, ty_fn]);
    let lbl_entry = b.alloc_id();
    let lbl_body = b.alloc_id();
    let lbl_lane_hdr = b.alloc_id();
    let lbl_lane_body = b.alloc_id();
    let lbl_in_elem = b.alloc_id();
    let lbl_k_hdr = b.alloc_id();
    let lbl_k_body = b.alloc_id();
    let lbl_k_cont = b.alloc_id();
    let lbl_k_exit = b.alloc_id();
    let lbl_lane_merge = b.alloc_id();
    let lbl_lane_cont = b.alloc_id();
    let lbl_lane_exit = b.alloc_id();
    let lbl_exit = b.alloc_id();

    b.inst(OP_LABEL, &[lbl_entry]);
    let var_lane_out = b.alloc_id();
    b.inst(OP_VARIABLE, &[ptr_fn_arr2, var_lane_out, SC_FUNCTION]);

    let gid_val = b.alloc_id();
    b.inst(OP_LOAD, &[ty_uvec3, gid_val, var_gid]);
    let pair_idx = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, pair_idx, gid_val, 0]);

    let p_m = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u0);
    let p_n = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u1);
    let p_k = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u2);
    let p_has_bias = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u3);
    let p_bias_len = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u4);
    let p_act = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u5);
    let p_batches = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u6);

    let mn = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, mn, p_m, p_n]);
    let total_elems = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, total_elems, p_batches, mn]);
    let tp_plus1 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, tp_plus1, total_elems, c_u1]);
    let total_pairs = b.alloc_id();
    b.inst(
        OP_SHIFT_RIGHT_LOGICAL,
        &[ty_u32, total_pairs, tp_plus1, c_u1],
    );

    let in_bounds = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, in_bounds, pair_idx, total_pairs]);
    b.inst(OP_SELECTION_MERGE, &[lbl_exit, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[in_bounds, lbl_body, lbl_exit]);

    b.inst(OP_LABEL, &[lbl_body]);
    let p_lo0 = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, p_lo0, var_lane_out, c_u0]);
    b.inst(OP_STORE, &[p_lo0, act_c.c_f0]);
    let p_lo1 = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, p_lo1, var_lane_out, c_u1]);
    b.inst(OP_STORE, &[p_lo1, act_c.c_f0]);
    let base_flat = b.alloc_id();
    b.inst(OP_SHIFT_LEFT_LOGICAL, &[ty_u32, base_flat, pair_idx, c_u1]);
    b.inst(OP_BRANCH, &[lbl_lane_hdr]);

    let lane_phi = b.alloc_id();
    let lane_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_lane_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, lane_phi, c_u0, lbl_body, lane_next, lbl_lane_cont],
    );
    let lane_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, lane_cond, lane_phi, c_u2]);
    b.inst(OP_LOOP_MERGE, &[lbl_lane_exit, lbl_lane_cont, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[lane_cond, lbl_lane_body, lbl_lane_exit],
    );

    b.inst(OP_LABEL, &[lbl_lane_body]);
    let flat = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, flat, base_flat, lane_phi]);
    let elem_ok = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, elem_ok, flat, total_elems]);
    b.inst(OP_SELECTION_MERGE, &[lbl_lane_merge, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[elem_ok, lbl_in_elem, lbl_lane_merge],
    );

    b.inst(OP_LABEL, &[lbl_in_elem]);
    let batch = b.alloc_id();
    b.inst(OP_UDIV, &[ty_u32, batch, flat, mn]);
    let rem = b.alloc_id();
    b.inst(OP_UMOD, &[ty_u32, rem, flat, mn]);
    let row = b.alloc_id();
    b.inst(OP_UDIV, &[ty_u32, row, rem, p_n]);
    let col = b.alloc_id();
    b.inst(OP_UMOD, &[ty_u32, col, rem, p_n]);
    let a_batch_base = b.load_buf(ptr_unif_u32, ty_u32, var_ab, c_u0, batch);
    let b_batch_base = b.load_buf(ptr_unif_u32, ty_u32, var_bb, c_u0, batch);
    let row_k = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, row_k, row, p_k]);
    let a_row_base = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, a_row_base, a_batch_base, row_k]);
    b.inst(OP_BRANCH, &[lbl_k_hdr]);

    let t_phi = b.alloc_id();
    let acc_phi = b.alloc_id();
    let t_next = b.alloc_id();
    let acc_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_k_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, t_phi, c_u0, lbl_in_elem, t_next, lbl_k_cont],
    );
    b.inst(
        OP_PHI,
        &[
            ty_f32,
            acc_phi,
            act_c.c_f0,
            lbl_in_elem,
            acc_next,
            lbl_k_cont,
        ],
    );
    let k_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, k_cond, t_phi, p_k]);
    b.inst(OP_LOOP_MERGE, &[lbl_k_exit, lbl_k_cont, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[k_cond, lbl_k_body, lbl_k_exit]);

    b.inst(OP_LABEL, &[lbl_k_body]);
    let a_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, a_idx, a_row_base, t_phi]);
    let a_val = unpack_half(&mut b, var_a, a_idx);
    let tn = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, tn, t_phi, p_n]);
    let b_i0 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, b_i0, b_batch_base, tn]);
    let b_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, b_idx, b_i0, col]);
    let b_val = unpack_half(&mut b, var_b, b_idx);
    let prod = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, prod, a_val, b_val]);
    b.inst(OP_FADD, &[ty_f32, acc_next, acc_phi, prod]);
    b.inst(OP_BRANCH, &[lbl_k_cont]);

    b.inst(OP_LABEL, &[lbl_k_cont]);
    b.inst(OP_IADD, &[ty_u32, t_next, t_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_k_hdr]);

    b.inst(OP_LABEL, &[lbl_k_exit]);
    let bias_idx = b.alloc_id();
    b.inst(OP_UMOD, &[ty_u32, bias_idx, col, p_bias_len]);
    let bias_val = unpack_half(&mut b, var_bias, bias_idx);
    let acc_plus_bias = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, acc_plus_bias, acc_phi, bias_val]);
    let has_bias_b = b.alloc_id();
    b.inst(OP_INOT_EQUAL, &[ty_bool, has_bias_b, p_has_bias, c_u0]);
    let x = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, x, has_bias_b, acc_plus_bias, acc_phi]);
    let act_out = emit_apply_act(&mut b, glsl_ext, ty_f32, ty_bool, &act_c, x, p_act);
    let p_lane_dst = b.alloc_id();
    b.inst(
        OP_ACCESS_CHAIN,
        &[ptr_fn_f32, p_lane_dst, var_lane_out, lane_phi],
    );
    b.inst(OP_STORE, &[p_lane_dst, act_out]);
    b.inst(OP_BRANCH, &[lbl_lane_merge]);

    b.inst(OP_LABEL, &[lbl_lane_merge]);
    b.inst(OP_BRANCH, &[lbl_lane_cont]);

    b.inst(OP_LABEL, &[lbl_lane_cont]);
    b.inst(OP_IADD, &[ty_u32, lane_next, lane_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_lane_hdr]);

    b.inst(OP_LABEL, &[lbl_lane_exit]);
    let v0 = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, v0, p_lo0]);
    let v1 = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, v1, p_lo1]);

    let packed = if is_bf16 {
        let pack_bf16 = |b: &mut SpirvBuilder, fv: u32| -> u32 {
            let bits = b.alloc_id();
            b.inst(OP_BITCAST, &[ty_u32, bits, fv]);
            let abs_b = b.alloc_id();
            b.inst(OP_BITWISE_AND, &[ty_u32, abs_b, bits, c_mask_abs]);
            let is_nan = b.alloc_id();
            b.inst(OP_UGREATER_THAN, &[ty_bool, is_nan, abs_b, c_inf]);
            let hi16 = b.alloc_id();
            b.inst(OP_SHIFT_RIGHT_LOGICAL, &[ty_u32, hi16, bits, c_u16]);
            let lsb = b.alloc_id();
            b.inst(OP_BITWISE_AND, &[ty_u32, lsb, hi16, c_u1]);
            let bias = b.alloc_id();
            b.inst(OP_IADD, &[ty_u32, bias, c_round_bias, lsb]);
            let rounded_raw = b.alloc_id();
            b.inst(OP_IADD, &[ty_u32, rounded_raw, bits, bias]);
            let rounded = b.alloc_id();
            b.inst(
                OP_SHIFT_RIGHT_LOGICAL,
                &[ty_u32, rounded, rounded_raw, c_u16],
            );
            let nan_h = b.alloc_id();
            b.inst(OP_BITWISE_OR, &[ty_u32, nan_h, hi16, c_qnan]);
            let chosen = b.alloc_id();
            b.inst(OP_SELECT, &[ty_u32, chosen, is_nan, nan_h, rounded]);
            let masked = b.alloc_id();
            b.inst(OP_BITWISE_AND, &[ty_u32, masked, chosen, c_mask_lo]);
            masked
        };
        let h0 = pack_bf16(&mut b, v0);
        let h1 = pack_bf16(&mut b, v1);
        let h1_sh = b.alloc_id();
        b.inst(OP_SHIFT_LEFT_LOGICAL, &[ty_u32, h1_sh, h1, c_u16]);
        let comb = b.alloc_id();
        b.inst(OP_BITWISE_OR, &[ty_u32, comb, h0, h1_sh]);
        comb
    } else {
        let vec2_val = b.alloc_id();
        b.inst(OP_COMPOSITE_CONSTRUCT, &[ty_vec2, vec2_val, v0, v1]);
        let pk = b.alloc_id();
        b.inst(
            OP_EXT_INST,
            &[ty_u32, pk, glsl_ext, GLSL_PACK_HALF_2X16, vec2_val],
        );
        pk
    };

    b.store_buf(ptr_unif_u32, var_out, c_u0, pair_idx, packed);
    b.inst(OP_BRANCH, &[lbl_exit]);

    b.inst(OP_LABEL, &[lbl_exit]);
    b.inst(OP_RETURN, &[]);
    b.inst(OP_FUNCTION_END, &[]);
    b.finish()
}

/// Shader 4 (4 SSBOs): Row-wise GPU RMSNorm `Out[r, t] = X[r, t] * inv_rms[r] * gamma[t]` in `f32`.
/// Bindings: `0=X(f32[])`, `1=Gamma(f32[])`, `2=Out(f32[])`,
/// `3=Params(u32[])` (`[M, K, has_gamma, gamma_len, eps_f32_bits]`).
pub(super) fn emit_spirv_rms_norm_f32() -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    b.inst(OP_CAPABILITY, &[1]);
    let glsl_ext = b.alloc_id();
    let mut ext_ops = vec![glsl_ext];
    SpirvBuilder::push_str_words(&mut ext_ops, "GLSL.std.450");
    b.inst(OP_EXT_INST_IMPORT, &ext_ops);
    b.inst(OP_MEMORY_MODEL, &[0, 1]);

    let main_fn = b.alloc_id();
    let var_gid = b.alloc_id();
    let mut ep_ops = vec![5, main_fn];
    SpirvBuilder::push_str_words(&mut ep_ops, "main");
    ep_ops.push(var_gid);
    b.inst(OP_ENTRY_POINT, &ep_ops);
    b.inst(OP_EXECUTION_MODE, &[main_fn, 17, 64, 1, 1]);

    let ty_f32_arr = b.alloc_id();
    let ty_f32_buf = b.alloc_id();
    let ty_u32_arr = b.alloc_id();
    let ty_u32_buf = b.alloc_id();
    let var_x = b.alloc_id();
    let var_gamma = b.alloc_id();
    let var_out = b.alloc_id();
    let var_params = b.alloc_id();

    b.inst(OP_DECORATE, &[var_gid, DEC_BUILT_IN, 28]);
    b.inst(OP_DECORATE, &[ty_f32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_f32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_f32_buf, 0, DEC_OFFSET, 0]);
    b.inst(OP_DECORATE, &[ty_u32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_u32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_u32_buf, 0, DEC_OFFSET, 0]);
    for (binding, var_id) in [(0u32, var_x), (1, var_gamma), (2, var_out), (3, var_params)] {
        b.inst(OP_DECORATE, &[var_id, DEC_DESCRIPTOR_SET, 0]);
        b.inst(OP_DECORATE, &[var_id, DEC_BINDING, binding]);
    }

    let ty_void = b.alloc_id();
    b.inst(OP_TYPE_VOID, &[ty_void]);
    let ty_fn = b.alloc_id();
    b.inst(OP_TYPE_FUNCTION, &[ty_fn, ty_void]);
    let ty_bool = b.alloc_id();
    b.inst(OP_TYPE_BOOL, &[ty_bool]);
    let ty_u32 = b.alloc_id();
    b.inst(OP_TYPE_INT, &[ty_u32, 32, 0]);
    let ty_f32 = b.alloc_id();
    b.inst(OP_TYPE_FLOAT, &[ty_f32, 32]);
    let ty_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_uvec3, ty_u32, 3]);
    let ptr_in_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_in_uvec3, SC_INPUT, ty_uvec3]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_f32_arr, ty_f32]);
    b.inst(OP_TYPE_STRUCT, &[ty_f32_buf, ty_f32_arr]);
    let ptr_unif_f32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_f32_buf, SC_UNIFORM, ty_f32_buf]);
    let ptr_unif_f32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_f32, SC_UNIFORM, ty_f32]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_u32_arr, ty_u32]);
    b.inst(OP_TYPE_STRUCT, &[ty_u32_buf, ty_u32_arr]);
    let ptr_unif_u32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32_buf, SC_UNIFORM, ty_u32_buf]);
    let ptr_unif_u32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32, SC_UNIFORM, ty_u32]);

    let c_u0 = b.c_u32(ty_u32, 0);
    let c_u1 = b.c_u32(ty_u32, 1);
    let c_u2 = b.c_u32(ty_u32, 2);
    let c_u3 = b.c_u32(ty_u32, 3);
    let c_u4 = b.c_u32(ty_u32, 4);
    let c_f0 = b.c_f32(ty_f32, 0.0);
    let c_f1 = b.c_f32(ty_f32, 1.0);

    b.inst(OP_VARIABLE, &[ptr_in_uvec3, var_gid, SC_INPUT]);
    b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, var_x, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, var_gamma, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, var_out, SC_UNIFORM]);
    b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, var_params, SC_UNIFORM]);

    b.inst(OP_FUNCTION, &[ty_void, main_fn, 0, ty_fn]);
    let lbl_entry = b.alloc_id();
    let lbl_body = b.alloc_id();
    let lbl_l1_hdr = b.alloc_id();
    let lbl_l1_body = b.alloc_id();
    let lbl_l1_cont = b.alloc_id();
    let lbl_l1_exit = b.alloc_id();
    let lbl_l2_hdr = b.alloc_id();
    let lbl_l2_body = b.alloc_id();
    let lbl_l2_cont = b.alloc_id();
    let lbl_l2_exit = b.alloc_id();
    let lbl_exit = b.alloc_id();

    b.inst(OP_LABEL, &[lbl_entry]);
    let gid_val = b.alloc_id();
    b.inst(OP_LOAD, &[ty_uvec3, gid_val, var_gid]);
    let row = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, row, gid_val, 0]);

    let p_m = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u0);
    let p_k = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u1);
    let p_has_gamma = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u2);
    let p_gamma_len = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u3);
    let p_eps_bits = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u4);
    let eps = b.alloc_id();
    b.inst(OP_BITCAST, &[ty_f32, eps, p_eps_bits]);

    let in_bounds = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, in_bounds, row, p_m]);
    b.inst(OP_SELECTION_MERGE, &[lbl_exit, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[in_bounds, lbl_body, lbl_exit]);

    b.inst(OP_LABEL, &[lbl_body]);
    let row_k = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, row_k, row, p_k]);
    b.inst(OP_BRANCH, &[lbl_l1_hdr]);

    let t1_phi = b.alloc_id();
    let sq_phi = b.alloc_id();
    let t1_next = b.alloc_id();
    let sq_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_l1_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, t1_phi, c_u0, lbl_body, t1_next, lbl_l1_cont],
    );
    b.inst(
        OP_PHI,
        &[ty_f32, sq_phi, c_f0, lbl_body, sq_next, lbl_l1_cont],
    );
    let l1_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, l1_cond, t1_phi, p_k]);
    b.inst(OP_LOOP_MERGE, &[lbl_l1_exit, lbl_l1_cont, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[l1_cond, lbl_l1_body, lbl_l1_exit]);

    b.inst(OP_LABEL, &[lbl_l1_body]);
    let x_idx1 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, x_idx1, row_k, t1_phi]);
    let xv1 = b.load_buf(ptr_unif_f32, ty_f32, var_x, c_u0, x_idx1);
    let xv_sq = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, xv_sq, xv1, xv1]);
    b.inst(OP_FADD, &[ty_f32, sq_next, sq_phi, xv_sq]);
    b.inst(OP_BRANCH, &[lbl_l1_cont]);

    b.inst(OP_LABEL, &[lbl_l1_cont]);
    b.inst(OP_IADD, &[ty_u32, t1_next, t1_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_l1_hdr]);

    b.inst(OP_LABEL, &[lbl_l1_exit]);
    let k_f = b.alloc_id();
    b.inst(OP_CONVERT_U_TO_F, &[ty_f32, k_f, p_k]);
    let ms = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, ms, sq_phi, k_f]);
    let ms_eps = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, ms_eps, ms, eps]);
    let rms = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, rms, glsl_ext, GLSL_SQRT, ms_eps]);
    let inv_rms = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, inv_rms, c_f1, rms]);
    let has_gamma_b = b.alloc_id();
    b.inst(OP_INOT_EQUAL, &[ty_bool, has_gamma_b, p_has_gamma, c_u0]);
    b.inst(OP_BRANCH, &[lbl_l2_hdr]);

    let t2_phi = b.alloc_id();
    let t2_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_l2_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, t2_phi, c_u0, lbl_l1_exit, t2_next, lbl_l2_cont],
    );
    let l2_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, l2_cond, t2_phi, p_k]);
    b.inst(OP_LOOP_MERGE, &[lbl_l2_exit, lbl_l2_cont, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[l2_cond, lbl_l2_body, lbl_l2_exit]);

    b.inst(OP_LABEL, &[lbl_l2_body]);
    let x_idx2 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, x_idx2, row_k, t2_phi]);
    let xv2 = b.load_buf(ptr_unif_f32, ty_f32, var_x, c_u0, x_idx2);
    let xn = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, xn, xv2, inv_rms]);
    let g_idx = b.alloc_id();
    b.inst(OP_UMOD, &[ty_u32, g_idx, t2_phi, p_gamma_len]);
    let g_raw = b.load_buf(ptr_unif_f32, ty_f32, var_gamma, c_u0, g_idx);
    let g_eff = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, g_eff, has_gamma_b, g_raw, c_f1]);
    let out_v = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, out_v, xn, g_eff]);
    b.store_buf(ptr_unif_f32, var_out, c_u0, x_idx2, out_v);
    b.inst(OP_BRANCH, &[lbl_l2_cont]);

    b.inst(OP_LABEL, &[lbl_l2_cont]);
    b.inst(OP_IADD, &[ty_u32, t2_next, t2_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_l2_hdr]);

    b.inst(OP_LABEL, &[lbl_l2_exit]);
    b.inst(OP_BRANCH, &[lbl_exit]);

    b.inst(OP_LABEL, &[lbl_exit]);
    b.inst(OP_RETURN, &[]);
    b.inst(OP_FUNCTION_END, &[]);
    b.finish()
}

/// Shader 5 (10 SSBOs): Single-pass Online-Softmax FlashAttention-2 in `f32` (zero O(Sq*Sk) VRAM).
/// Bindings: `0=Q`, `1=K`, `2=V`, `3=Mask`, `4=Out` (`f32[]`),
/// `5=QBases`, `6=KBases`, `7=VBases`, `8=MaskBases`, `9=Params` (`u32[]`).
/// `Params = [batches, s_q, s_k, d_k, d_v, k_transposed, has_mask, mask_sq_s, mask_sk_s, scale_bits]`.
pub(super) fn emit_spirv_flash_attention_f32() -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    b.inst(OP_CAPABILITY, &[1]);
    let glsl_ext = b.alloc_id();
    let mut ext_ops = vec![glsl_ext];
    SpirvBuilder::push_str_words(&mut ext_ops, "GLSL.std.450");
    b.inst(OP_EXT_INST_IMPORT, &ext_ops);
    b.inst(OP_MEMORY_MODEL, &[0, 1]);

    let main_fn = b.alloc_id();
    let var_gid = b.alloc_id();
    let mut ep_ops = vec![5, main_fn];
    SpirvBuilder::push_str_words(&mut ep_ops, "main");
    ep_ops.push(var_gid);
    b.inst(OP_ENTRY_POINT, &ep_ops);
    b.inst(OP_EXECUTION_MODE, &[main_fn, 17, 16, 16, 1]);

    let ty_f32_arr = b.alloc_id();
    let ty_f32_buf = b.alloc_id();
    let ty_u32_arr = b.alloc_id();
    let ty_u32_buf = b.alloc_id();
    let var_q = b.alloc_id();
    let var_k = b.alloc_id();
    let var_v = b.alloc_id();
    let var_mask = b.alloc_id();
    let var_out = b.alloc_id();
    let var_qb = b.alloc_id();
    let var_kb = b.alloc_id();
    let var_vb = b.alloc_id();
    let var_mb = b.alloc_id();
    let var_params = b.alloc_id();

    b.inst(OP_DECORATE, &[var_gid, DEC_BUILT_IN, 28]);
    b.inst(OP_DECORATE, &[ty_f32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_f32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_f32_buf, 0, DEC_OFFSET, 0]);
    b.inst(OP_DECORATE, &[ty_u32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_u32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_u32_buf, 0, DEC_OFFSET, 0]);
    for (binding, var_id) in [
        (0u32, var_q),
        (1, var_k),
        (2, var_v),
        (3, var_mask),
        (4, var_out),
        (5, var_qb),
        (6, var_kb),
        (7, var_vb),
        (8, var_mb),
        (9, var_params),
    ] {
        b.inst(OP_DECORATE, &[var_id, DEC_DESCRIPTOR_SET, 0]);
        b.inst(OP_DECORATE, &[var_id, DEC_BINDING, binding]);
    }

    let ty_void = b.alloc_id();
    b.inst(OP_TYPE_VOID, &[ty_void]);
    let ty_fn = b.alloc_id();
    b.inst(OP_TYPE_FUNCTION, &[ty_fn, ty_void]);
    let ty_bool = b.alloc_id();
    b.inst(OP_TYPE_BOOL, &[ty_bool]);
    let ty_u32 = b.alloc_id();
    b.inst(OP_TYPE_INT, &[ty_u32, 32, 0]);
    let ty_f32 = b.alloc_id();
    b.inst(OP_TYPE_FLOAT, &[ty_f32, 32]);
    let ty_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_uvec3, ty_u32, 3]);
    let ptr_in_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_in_uvec3, SC_INPUT, ty_uvec3]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_f32_arr, ty_f32]);
    b.inst(OP_TYPE_STRUCT, &[ty_f32_buf, ty_f32_arr]);
    let ptr_unif_f32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_f32_buf, SC_UNIFORM, ty_f32_buf]);
    let ptr_unif_f32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_f32, SC_UNIFORM, ty_f32]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_u32_arr, ty_u32]);
    b.inst(OP_TYPE_STRUCT, &[ty_u32_buf, ty_u32_arr]);
    let ptr_unif_u32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32_buf, SC_UNIFORM, ty_u32_buf]);
    let ptr_unif_u32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32, SC_UNIFORM, ty_u32]);

    let c_u: Vec<u32> = (0..10).map(|i| b.c_u32(ty_u32, i)).collect();
    let c_f0 = b.c_f32(ty_f32, 0.0);
    let c_f_neginf = b.c_f32(ty_f32, -3.4e38_f32);
    let c_f_mask_cut = b.c_f32(ty_f32, -1.0e30_f32);
    let c_f_exp_lo = b.c_f32(ty_f32, -80.0);

    b.inst(OP_VARIABLE, &[ptr_in_uvec3, var_gid, SC_INPUT]);
    for v in [var_q, var_k, var_v, var_mask, var_out] {
        b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, v, SC_UNIFORM]);
    }
    for v in [var_qb, var_kb, var_vb, var_mb, var_params] {
        b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, v, SC_UNIFORM]);
    }

    b.inst(OP_FUNCTION, &[ty_void, main_fn, 0, ty_fn]);
    let lbl_entry = b.alloc_id();
    let lbl_body = b.alloc_id();
    let lbl_k_hdr = b.alloc_id();
    let lbl_k_body = b.alloc_id();
    let lbl_d_hdr = b.alloc_id();
    let lbl_d_body = b.alloc_id();
    let lbl_d_cont = b.alloc_id();
    let lbl_d_exit = b.alloc_id();
    let lbl_k_cont = b.alloc_id();
    let lbl_k_exit = b.alloc_id();
    let lbl_exit = b.alloc_id();

    b.inst(OP_LABEL, &[lbl_entry]);
    let gid_val = b.alloc_id();
    b.inst(OP_LOAD, &[ty_uvec3, gid_val, var_gid]);
    let di = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, di, gid_val, 0]);
    let qi = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, qi, gid_val, 1]);
    let batch = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, batch, gid_val, 2]);

    let p_batches = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[0]);
    let p_sq = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[1]);
    let p_sk = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[2]);
    let p_dk = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[3]);
    let p_dv = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[4]);
    let p_kt = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[5]);
    let p_hm = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[6]);
    let p_msq = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[7]);
    let p_msk = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[8]);
    let p_scale_bits = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[9]);
    let scale = b.alloc_id();
    b.inst(OP_BITCAST, &[ty_f32, scale, p_scale_bits]);

    let c0 = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, c0, qi, p_sq]);
    let c1 = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, c1, di, p_dv]);
    let c2 = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, c2, batch, p_batches]);
    let a0 = b.alloc_id();
    b.inst(OP_LOGICAL_AND, &[ty_bool, a0, c0, c1]);
    let in_bounds = b.alloc_id();
    b.inst(OP_LOGICAL_AND, &[ty_bool, in_bounds, a0, c2]);
    b.inst(OP_SELECTION_MERGE, &[lbl_exit, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[in_bounds, lbl_body, lbl_exit]);

    b.inst(OP_LABEL, &[lbl_body]);
    let q_base0 = b.load_buf(ptr_unif_u32, ty_u32, var_qb, c_u[0], batch);
    let k_base = b.load_buf(ptr_unif_u32, ty_u32, var_kb, c_u[0], batch);
    let v_base = b.load_buf(ptr_unif_u32, ty_u32, var_vb, c_u[0], batch);
    let m_base = b.load_buf(ptr_unif_u32, ty_u32, var_mb, c_u[0], batch);
    let qi_dk = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, qi_dk, qi, p_dk]);
    let q_row_base = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, q_row_base, q_base0, qi_dk]);
    let qi_msq = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, qi_msq, qi, p_msq]);
    let mask_row_base = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, mask_row_base, m_base, qi_msq]);
    let is_kt = b.alloc_id();
    b.inst(OP_INOT_EQUAL, &[ty_bool, is_kt, p_kt, c_u[0]]);
    let has_mask_b = b.alloc_id();
    b.inst(OP_INOT_EQUAL, &[ty_bool, has_mask_b, p_hm, c_u[0]]);
    b.inst(OP_BRANCH, &[lbl_k_hdr]);

    let kj_phi = b.alloc_id();
    let m_phi = b.alloc_id();
    let l_phi = b.alloc_id();
    let acc_phi = b.alloc_id();
    let kj_next = b.alloc_id();
    let m_next = b.alloc_id();
    let l_next = b.alloc_id();
    let acc_next = b.alloc_id();

    b.inst(OP_LABEL, &[lbl_k_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, kj_phi, c_u[0], lbl_body, kj_next, lbl_k_cont],
    );
    b.inst(
        OP_PHI,
        &[ty_f32, m_phi, c_f_neginf, lbl_body, m_next, lbl_k_cont],
    );
    b.inst(OP_PHI, &[ty_f32, l_phi, c_f0, lbl_body, l_next, lbl_k_cont]);
    b.inst(
        OP_PHI,
        &[ty_f32, acc_phi, c_f0, lbl_body, acc_next, lbl_k_cont],
    );
    let k_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, k_cond, kj_phi, p_sk]);
    b.inst(OP_LOOP_MERGE, &[lbl_k_exit, lbl_k_cont, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[k_cond, lbl_k_body, lbl_k_exit]);

    b.inst(OP_LABEL, &[lbl_k_body]);
    let kj_dk = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, kj_dk, kj_phi, p_dk]);
    let k_norm_row = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, k_norm_row, k_base, kj_dk]);
    b.inst(OP_BRANCH, &[lbl_d_hdr]);

    let t_phi = b.alloc_id();
    let dot_phi = b.alloc_id();
    let t_next = b.alloc_id();
    let dot_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_d_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, t_phi, c_u[0], lbl_k_body, t_next, lbl_d_cont],
    );
    b.inst(
        OP_PHI,
        &[ty_f32, dot_phi, c_f0, lbl_k_body, dot_next, lbl_d_cont],
    );
    let d_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, d_cond, t_phi, p_dk]);
    b.inst(OP_LOOP_MERGE, &[lbl_d_exit, lbl_d_cont, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[d_cond, lbl_d_body, lbl_d_exit]);

    b.inst(OP_LABEL, &[lbl_d_body]);
    let q_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, q_idx, q_row_base, t_phi]);
    let qv = b.load_buf(ptr_unif_f32, ty_f32, var_q, c_u[0], q_idx);

    let k_idx_norm = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, k_idx_norm, k_norm_row, t_phi]);
    let t_sk = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, t_sk, t_phi, p_sk]);
    let k_t0 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, k_t0, k_base, t_sk]);
    let k_idx_tr = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, k_idx_tr, k_t0, kj_phi]);
    let k_idx = b.alloc_id();
    b.inst(OP_SELECT, &[ty_u32, k_idx, is_kt, k_idx_tr, k_idx_norm]);
    let kv = b.load_buf(ptr_unif_f32, ty_f32, var_k, c_u[0], k_idx);

    let prod = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, prod, qv, kv]);
    b.inst(OP_FADD, &[ty_f32, dot_next, dot_phi, prod]);
    b.inst(OP_BRANCH, &[lbl_d_cont]);

    b.inst(OP_LABEL, &[lbl_d_cont]);
    b.inst(OP_IADD, &[ty_u32, t_next, t_phi, c_u[1]]);
    b.inst(OP_BRANCH, &[lbl_d_hdr]);

    b.inst(OP_LABEL, &[lbl_d_exit]);
    let score0 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, score0, dot_phi, scale]);

    let kj_msk = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, kj_msk, kj_phi, p_msk]);
    let mask_idx_raw = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, mask_idx_raw, mask_row_base, kj_msk]);
    let mask_idx = b.alloc_id();
    b.inst(
        OP_SELECT,
        &[ty_u32, mask_idx, has_mask_b, mask_idx_raw, c_u[0]],
    );
    let mask_v = b.load_buf(ptr_unif_f32, ty_f32, var_mask, c_u[0], mask_idx);
    let score_m = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, score_m, score0, mask_v]);
    let score = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, score, has_mask_b, score_m, score0]);

    let valid = b.alloc_id();
    b.inst(OP_FORD_GREATER_THAN, &[ty_bool, valid, score, c_f_mask_cut]);
    let eff_score = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, eff_score, valid, score, m_phi]);
    let m_cand = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, m_cand, glsl_ext, GLSL_FMAX, m_phi, eff_score],
    );
    b.inst(OP_SELECT, &[ty_f32, m_next, valid, m_cand, m_phi]);

    let l_zero = b.alloc_id();
    b.inst(OP_FORD_EQUAL, &[ty_bool, l_zero, l_phi, c_f0]);
    let dm = b.alloc_id();
    b.inst(OP_FSUB, &[ty_f32, dm, m_phi, m_next]);
    let dm_c = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, dm_c, glsl_ext, GLSL_FCLAMP, dm, c_f_exp_lo, c_f0],
    );
    let exp_dm = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, exp_dm, glsl_ext, GLSL_EXP, dm_c]);
    let alpha = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, alpha, l_zero, c_f0, exp_dm]);

    let ds = b.alloc_id();
    b.inst(OP_FSUB, &[ty_f32, ds, eff_score, m_next]);
    let ds_c = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, ds_c, glsl_ext, GLSL_FCLAMP, ds, c_f_exp_lo, c_f0],
    );
    let exp_ds = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, exp_ds, glsl_ext, GLSL_EXP, ds_c]);
    let p_w = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, p_w, valid, exp_ds, c_f0]);

    let kj_dv = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, kj_dv, kj_phi, p_dv]);
    let v_i0 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, v_i0, v_base, kj_dv]);
    let v_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, v_idx, v_i0, di]);
    let vv = b.load_buf(ptr_unif_f32, ty_f32, var_v, c_u[0], v_idx);

    let l_scaled = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, l_scaled, l_phi, alpha]);
    let l_cand = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, l_cand, l_scaled, p_w]);
    b.inst(OP_SELECT, &[ty_f32, l_next, valid, l_cand, l_phi]);

    let acc_scaled = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, acc_scaled, acc_phi, alpha]);
    let pv = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, pv, p_w, vv]);
    let acc_cand = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, acc_cand, acc_scaled, pv]);
    b.inst(OP_SELECT, &[ty_f32, acc_next, valid, acc_cand, acc_phi]);
    b.inst(OP_BRANCH, &[lbl_k_cont]);

    b.inst(OP_LABEL, &[lbl_k_cont]);
    b.inst(OP_IADD, &[ty_u32, kj_next, kj_phi, c_u[1]]);
    b.inst(OP_BRANCH, &[lbl_k_hdr]);

    b.inst(OP_LABEL, &[lbl_k_exit]);
    let l_pos = b.alloc_id();
    b.inst(OP_FORD_GREATER_THAN, &[ty_bool, l_pos, l_phi, c_f0]);
    let acc_div = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, acc_div, acc_phi, l_phi]);
    let out_val = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, out_val, l_pos, acc_div, c_f0]);

    let sq_dv = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, sq_dv, p_sq, p_dv]);
    let b_off = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, b_off, batch, sq_dv]);
    let qi_dv = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, qi_dv, qi, p_dv]);
    let oi0 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, oi0, b_off, qi_dv]);
    let out_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, out_idx, oi0, di]);
    b.store_buf(ptr_unif_f32, var_out, c_u[0], out_idx, out_val);
    b.inst(OP_BRANCH, &[lbl_exit]);

    b.inst(OP_LABEL, &[lbl_exit]);
    b.inst(OP_RETURN, &[]);
    b.inst(OP_FUNCTION_END, &[]);
    b.finish()
}

/// Shader 6 (11 SSBOs): Single-launch 25-opcode bytecode-tape FusedElementwise broadcast evaluator
/// (`<= 64` live slots, `<= 16` output dims).
/// Bindings: `0=Out`, `1=In0`, `2=In1`, `3=In2`, `4=In3` (`f32[]`),
/// `5=InStrides`, `6=OutShape`, `7=Ops`, `8=ArgA`, `9=ArgB`, `10=Meta` (`u32[]`).
/// `Meta = [ndim, n_inputs, n_steps, numel, packed_mode, imm_bits[0..n_steps], in_offsets[0..n_inputs]]`.
pub(super) fn emit_spirv_fused_elementwise_f32() -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    b.inst(OP_CAPABILITY, &[1]);
    let glsl_ext = b.alloc_id();
    let mut ext_ops = vec![glsl_ext];
    SpirvBuilder::push_str_words(&mut ext_ops, "GLSL.std.450");
    b.inst(OP_EXT_INST_IMPORT, &ext_ops);
    b.inst(OP_MEMORY_MODEL, &[0, 1]);

    let main_fn = b.alloc_id();
    let var_gid = b.alloc_id();
    let mut ep_ops = vec![5, main_fn];
    SpirvBuilder::push_str_words(&mut ep_ops, "main");
    ep_ops.push(var_gid);
    b.inst(OP_ENTRY_POINT, &ep_ops);
    b.inst(OP_EXECUTION_MODE, &[main_fn, 17, 64, 1, 1]);

    let ty_f32_arr = b.alloc_id();
    let ty_f32_buf = b.alloc_id();
    let ty_u32_arr = b.alloc_id();
    let ty_u32_buf = b.alloc_id();
    let var_out = b.alloc_id();
    let var_in0 = b.alloc_id();
    let var_in1 = b.alloc_id();
    let var_in2 = b.alloc_id();
    let var_in3 = b.alloc_id();
    let var_strides = b.alloc_id();
    let var_oshape = b.alloc_id();
    let var_ops = b.alloc_id();
    let var_arga = b.alloc_id();
    let var_argb = b.alloc_id();
    let var_meta = b.alloc_id();

    b.inst(OP_DECORATE, &[var_gid, DEC_BUILT_IN, 28]);
    b.inst(OP_DECORATE, &[ty_f32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_f32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_f32_buf, 0, DEC_OFFSET, 0]);
    b.inst(OP_DECORATE, &[ty_u32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_u32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_u32_buf, 0, DEC_OFFSET, 0]);
    for (binding, var_id) in [
        (0u32, var_out),
        (1, var_in0),
        (2, var_in1),
        (3, var_in2),
        (4, var_in3),
        (5, var_strides),
        (6, var_oshape),
        (7, var_ops),
        (8, var_arga),
        (9, var_argb),
        (10, var_meta),
    ] {
        b.inst(OP_DECORATE, &[var_id, DEC_DESCRIPTOR_SET, 0]);
        b.inst(OP_DECORATE, &[var_id, DEC_BINDING, binding]);
    }

    let ty_void = b.alloc_id();
    b.inst(OP_TYPE_VOID, &[ty_void]);
    let ty_fn = b.alloc_id();
    b.inst(OP_TYPE_FUNCTION, &[ty_fn, ty_void]);
    let ty_bool = b.alloc_id();
    b.inst(OP_TYPE_BOOL, &[ty_bool]);
    let ty_u32 = b.alloc_id();
    b.inst(OP_TYPE_INT, &[ty_u32, 32, 0]);
    let ty_f32 = b.alloc_id();
    b.inst(OP_TYPE_FLOAT, &[ty_f32, 32]);
    let ty_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_uvec3, ty_u32, 3]);
    let ptr_in_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_in_uvec3, SC_INPUT, ty_uvec3]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_f32_arr, ty_f32]);
    b.inst(OP_TYPE_STRUCT, &[ty_f32_buf, ty_f32_arr]);
    let ptr_unif_f32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_f32_buf, SC_UNIFORM, ty_f32_buf]);
    let ptr_unif_f32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_f32, SC_UNIFORM, ty_f32]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_u32_arr, ty_u32]);
    b.inst(OP_TYPE_STRUCT, &[ty_u32_buf, ty_u32_arr]);
    let ptr_unif_u32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32_buf, SC_UNIFORM, ty_u32_buf]);
    let ptr_unif_u32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32, SC_UNIFORM, ty_u32]);

    let c_u0 = b.c_u32(ty_u32, 0);
    let c_u1 = b.c_u32(ty_u32, 1);
    let c_u2 = b.c_u32(ty_u32, 2);
    let c_u3 = b.c_u32(ty_u32, 3);
    let c_u4 = b.c_u32(ty_u32, 4);
    let c_u5 = b.c_u32(ty_u32, 5);
    let c_u16 = b.c_u32(ty_u32, 16);
    let c_u64 = b.c_u32(ty_u32, 64);

    let ty_slots = b.alloc_id();
    b.inst(OP_TYPE_ARRAY, &[ty_slots, ty_f32, c_u64]);
    let ptr_fn_slots = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_slots, SC_FUNCTION, ty_slots]);
    let ty_coords = b.alloc_id();
    b.inst(OP_TYPE_ARRAY, &[ty_coords, ty_u32, c_u16]);
    let ptr_fn_coords = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_coords, SC_FUNCTION, ty_coords]);
    let ptr_fn_f32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_f32, SC_FUNCTION, ty_f32]);
    let ptr_fn_u32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_u32, SC_FUNCTION, ty_u32]);

    let c_f0 = b.c_f32(ty_f32, 0.0);
    let c_f1 = b.c_f32(ty_f32, 1.0);
    let c_f2 = b.c_f32(ty_f32, 2.0);
    let c_f3 = b.c_f32(ty_f32, 3.0);
    let c_f05 = b.c_f32(ty_f32, 0.5);
    let c_gelu_a = b.c_f32(ty_f32, 0.797_884_6);
    let c_gelu_b = b.c_f32(ty_f32, 0.044_715);
    let c_f_lo = b.c_f32(ty_f32, -40.0);
    let c_f_hi = b.c_f32(ty_f32, 40.0);

    b.inst(OP_VARIABLE, &[ptr_in_uvec3, var_gid, SC_INPUT]);
    for v in [var_out, var_in0, var_in1, var_in2, var_in3] {
        b.inst(OP_VARIABLE, &[ptr_unif_f32_buf, v, SC_UNIFORM]);
    }
    for v in [
        var_strides,
        var_oshape,
        var_ops,
        var_arga,
        var_argb,
        var_meta,
    ] {
        b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, v, SC_UNIFORM]);
    }

    b.inst(OP_FUNCTION, &[ty_void, main_fn, 0, ty_fn]);
    let lbl_entry = b.alloc_id();
    let lbl_body = b.alloc_id();
    let lbl_dec_hdr = b.alloc_id();
    let lbl_dec_body = b.alloc_id();
    let lbl_dec_cont = b.alloc_id();
    let lbl_dec_exit = b.alloc_id();
    let lbl_in_hdr = b.alloc_id();
    let lbl_in_body = b.alloc_id();
    let lbl_st_hdr = b.alloc_id();
    let lbl_st_body = b.alloc_id();
    let lbl_st_cont = b.alloc_id();
    let lbl_st_exit = b.alloc_id();
    let lbl_in_packed = b.alloc_id();
    let lbl_in_direct = b.alloc_id();
    let lbl_in_c0 = b.alloc_id();
    let lbl_in_c1 = b.alloc_id();
    let lbl_in_c2 = b.alloc_id();
    let lbl_in_c3 = b.alloc_id();
    let lbl_in_sw_merge = b.alloc_id();
    let lbl_in_fetch_merge = b.alloc_id();
    let lbl_in_cont = b.alloc_id();
    let lbl_in_exit = b.alloc_id();
    let lbl_step_hdr = b.alloc_id();
    let lbl_step_body = b.alloc_id();
    let lbl_step_merge = b.alloc_id();
    let lbl_step_cont = b.alloc_id();
    let lbl_step_exit = b.alloc_id();
    let lbl_exit = b.alloc_id();
    let op_lbls: Vec<u32> = (0..25).map(|_| b.alloc_id()).collect();

    b.inst(OP_LABEL, &[lbl_entry]);
    // All SC_FUNCTION local variables MUST be at the start of the entry block!
    let var_slots = b.alloc_id();
    b.inst(OP_VARIABLE, &[ptr_fn_slots, var_slots, SC_FUNCTION]);
    let var_coords = b.alloc_id();
    b.inst(OP_VARIABLE, &[ptr_fn_coords, var_coords, SC_FUNCTION]);
    let var_in_val = b.alloc_id();
    b.inst(OP_VARIABLE, &[ptr_fn_f32, var_in_val, SC_FUNCTION]);
    let var_step_res = b.alloc_id();
    b.inst(OP_VARIABLE, &[ptr_fn_f32, var_step_res, SC_FUNCTION]);

    let gid_val = b.alloc_id();
    b.inst(OP_LOAD, &[ty_uvec3, gid_val, var_gid]);
    let tid = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, tid, gid_val, 0]);

    let p_ndim = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, c_u0);
    let p_n_in = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, c_u1);
    let p_n_steps = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, c_u2);
    let p_numel = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, c_u3);
    let p_packed = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, c_u4);
    let is_packed = b.alloc_id();
    b.inst(OP_INOT_EQUAL, &[ty_bool, is_packed, p_packed, c_u0]);
    let offsets_base = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, offsets_base, c_u5, p_n_steps]);

    let in_bounds = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, in_bounds, tid, p_numel]);
    b.inst(OP_SELECTION_MERGE, &[lbl_exit, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[in_bounds, lbl_body, lbl_exit]);

    // Decompose `tid` into `coords[0..ndim]`
    b.inst(OP_LABEL, &[lbl_body]);
    b.inst(OP_BRANCH, &[lbl_dec_hdr]);

    let d_rem_phi = b.alloc_id();
    let rem_phi = b.alloc_id();
    let d_rem_next = b.alloc_id();
    let rem_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_dec_hdr]);
    b.inst(
        OP_PHI,
        &[
            ty_u32,
            d_rem_phi,
            p_ndim,
            lbl_body,
            d_rem_next,
            lbl_dec_cont,
        ],
    );
    b.inst(
        OP_PHI,
        &[ty_u32, rem_phi, tid, lbl_body, rem_next, lbl_dec_cont],
    );
    let dec_cond = b.alloc_id();
    b.inst(OP_UGREATER_THAN, &[ty_bool, dec_cond, d_rem_phi, c_u0]);
    b.inst(OP_LOOP_MERGE, &[lbl_dec_exit, lbl_dec_cont, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[dec_cond, lbl_dec_body, lbl_dec_exit],
    );

    b.inst(OP_LABEL, &[lbl_dec_body]);
    b.inst(OP_ISUB, &[ty_u32, d_rem_next, d_rem_phi, c_u1]);
    let dim_sz = b.load_buf(ptr_unif_u32, ty_u32, var_oshape, c_u0, d_rem_next);
    let coord = b.alloc_id();
    b.inst(OP_UMOD, &[ty_u32, coord, rem_phi, dim_sz]);
    b.inst(OP_UDIV, &[ty_u32, rem_next, rem_phi, dim_sz]);
    let pc = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_u32, pc, var_coords, d_rem_next]);
    b.inst(OP_STORE, &[pc, coord]);
    b.inst(OP_BRANCH, &[lbl_dec_cont]);

    b.inst(OP_LABEL, &[lbl_dec_cont]);
    b.inst(OP_BRANCH, &[lbl_dec_hdr]);

    // Load inputs into `slots[0..n_inputs]`
    b.inst(OP_LABEL, &[lbl_dec_exit]);
    b.inst(OP_BRANCH, &[lbl_in_hdr]);

    let inp_phi = b.alloc_id();
    let inp_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_in_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, inp_phi, c_u0, lbl_dec_exit, inp_next, lbl_in_cont],
    );
    let in_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, in_cond, inp_phi, p_n_in]);
    b.inst(OP_LOOP_MERGE, &[lbl_in_exit, lbl_in_cont, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[in_cond, lbl_in_body, lbl_in_exit]);

    b.inst(OP_LABEL, &[lbl_in_body]);
    let inp_ndim = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, inp_ndim, inp_phi, p_ndim]);
    b.inst(OP_BRANCH, &[lbl_st_hdr]);

    let sd_phi = b.alloc_id();
    let off_phi = b.alloc_id();
    let sd_next = b.alloc_id();
    let off_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_st_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, sd_phi, c_u0, lbl_in_body, sd_next, lbl_st_cont],
    );
    b.inst(
        OP_PHI,
        &[ty_u32, off_phi, c_u0, lbl_in_body, off_next, lbl_st_cont],
    );
    let st_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, st_cond, sd_phi, p_ndim]);
    b.inst(OP_LOOP_MERGE, &[lbl_st_exit, lbl_st_cont, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[st_cond, lbl_st_body, lbl_st_exit]);

    b.inst(OP_LABEL, &[lbl_st_body]);
    let pc2 = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_u32, pc2, var_coords, sd_phi]);
    let cv = b.alloc_id();
    b.inst(OP_LOAD, &[ty_u32, cv, pc2]);
    let s_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, s_idx, inp_ndim, sd_phi]);
    let stride = b.load_buf(ptr_unif_u32, ty_u32, var_strides, c_u0, s_idx);
    let term = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, term, cv, stride]);
    b.inst(OP_IADD, &[ty_u32, off_next, off_phi, term]);
    b.inst(OP_BRANCH, &[lbl_st_cont]);

    b.inst(OP_LABEL, &[lbl_st_cont]);
    b.inst(OP_IADD, &[ty_u32, sd_next, sd_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_st_hdr]);

    b.inst(OP_LABEL, &[lbl_st_exit]);
    b.inst(OP_SELECTION_MERGE, &[lbl_in_fetch_merge, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[is_packed, lbl_in_packed, lbl_in_direct],
    );

    b.inst(OP_LABEL, &[lbl_in_packed]);
    let ob_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, ob_idx, offsets_base, inp_phi]);
    let base_off = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, ob_idx);
    let pk_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, pk_idx, base_off, off_phi]);
    let pk_val = b.load_buf(ptr_unif_f32, ty_f32, var_in0, c_u0, pk_idx);
    b.inst(OP_STORE, &[var_in_val, pk_val]);
    b.inst(OP_BRANCH, &[lbl_in_fetch_merge]);

    b.inst(OP_LABEL, &[lbl_in_direct]);
    b.inst(OP_SELECTION_MERGE, &[lbl_in_sw_merge, 0]);
    b.inst(
        OP_SWITCH,
        &[inp_phi, lbl_in_c3, 0, lbl_in_c0, 1, lbl_in_c1, 2, lbl_in_c2],
    );
    for (lbl, var_in) in [
        (lbl_in_c0, var_in0),
        (lbl_in_c1, var_in1),
        (lbl_in_c2, var_in2),
        (lbl_in_c3, var_in3),
    ] {
        b.inst(OP_LABEL, &[lbl]);
        let v = b.load_buf(ptr_unif_f32, ty_f32, var_in, c_u0, off_phi);
        b.inst(OP_STORE, &[var_in_val, v]);
        b.inst(OP_BRANCH, &[lbl_in_sw_merge]);
    }
    b.inst(OP_LABEL, &[lbl_in_sw_merge]);
    b.inst(OP_BRANCH, &[lbl_in_fetch_merge]);

    b.inst(OP_LABEL, &[lbl_in_fetch_merge]);
    let loaded_in = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, loaded_in, var_in_val]);
    let ps_in = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, ps_in, var_slots, inp_phi]);
    b.inst(OP_STORE, &[ps_in, loaded_in]);
    b.inst(OP_BRANCH, &[lbl_in_cont]);

    b.inst(OP_LABEL, &[lbl_in_cont]);
    b.inst(OP_IADD, &[ty_u32, inp_next, inp_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_in_hdr]);

    // Evaluate bytecode steps `s in 0..n_steps`
    b.inst(OP_LABEL, &[lbl_in_exit]);
    b.inst(OP_BRANCH, &[lbl_step_hdr]);

    let s_phi = b.alloc_id();
    let s_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_step_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, s_phi, c_u0, lbl_in_exit, s_next, lbl_step_cont],
    );
    let s_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, s_cond, s_phi, p_n_steps]);
    b.inst(OP_LOOP_MERGE, &[lbl_step_exit, lbl_step_cont, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[s_cond, lbl_step_body, lbl_step_exit],
    );

    b.inst(OP_LABEL, &[lbl_step_body]);
    let op_v = b.load_buf(ptr_unif_u32, ty_u32, var_ops, c_u0, s_phi);
    let a_slot = b.load_buf(ptr_unif_u32, ty_u32, var_arga, c_u0, s_phi);
    let b_slot = b.load_buf(ptr_unif_u32, ty_u32, var_argb, c_u0, s_phi);
    let imm_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, imm_idx, c_u5, s_phi]);
    let imm_bits = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, imm_idx);
    let imm = b.alloc_id();
    b.inst(OP_BITCAST, &[ty_f32, imm, imm_bits]);

    let pa = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, pa, var_slots, a_slot]);
    let va = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, va, pa]);
    let pb = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, pb, var_slots, b_slot]);
    let vb = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, vb, pb]);

    // Helper for safe `pow(base, exp)` handling square/cube/zero cleanly for negative bases
    let emit_safe_pow = |b: &mut SpirvBuilder, base: u32, exp: u32| -> u32 {
        let is_sq = b.alloc_id();
        b.inst(OP_FORD_EQUAL, &[ty_bool, is_sq, exp, c_f2]);
        let is_cu = b.alloc_id();
        b.inst(OP_FORD_EQUAL, &[ty_bool, is_cu, exp, c_f3]);
        let sq = b.alloc_id();
        b.inst(OP_FMUL, &[ty_f32, sq, base, base]);
        let cu = b.alloc_id();
        b.inst(OP_FMUL, &[ty_f32, cu, sq, base]);
        let abs_b = b.alloc_id();
        b.inst(OP_EXT_INST, &[ty_f32, abs_b, glsl_ext, GLSL_FABS, base]);
        let is_z = b.alloc_id();
        b.inst(OP_FORD_EQUAL, &[ty_bool, is_z, abs_b, c_f0]);
        let safe_b = b.alloc_id();
        b.inst(OP_SELECT, &[ty_f32, safe_b, is_z, c_f1, abs_b]);
        let gen_p = b.alloc_id();
        b.inst(
            OP_EXT_INST,
            &[ty_f32, gen_p, glsl_ext, GLSL_POW, safe_b, exp],
        );
        let gen_z = b.alloc_id();
        b.inst(OP_SELECT, &[ty_f32, gen_z, is_z, c_f0, gen_p]);
        let r1 = b.alloc_id();
        b.inst(OP_SELECT, &[ty_f32, r1, is_cu, cu, gen_z]);
        let r2 = b.alloc_id();
        b.inst(OP_SELECT, &[ty_f32, r2, is_sq, sq, r1]);
        r2
    };

    // Structured 25-way OpSwitch over opcodes 0..=24 (default = 24 Abs)
    let mut sw_ops = vec![op_v, op_lbls[24]];
    for code in 0..24u32 {
        sw_ops.push(code);
        sw_ops.push(op_lbls[code as usize]);
    }
    b.inst(OP_SELECTION_MERGE, &[lbl_step_merge, 0]);
    b.inst(OP_SWITCH, &sw_ops);

    let finish_case = |b: &mut SpirvBuilder, res: u32| {
        b.inst(OP_STORE, &[var_step_res, res]);
        b.inst(OP_BRANCH, &[lbl_step_merge]);
    };

    // 0: Add
    b.inst(OP_LABEL, &[op_lbls[0]]);
    let r = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, r, va, vb]);
    finish_case(&mut b, r);

    // 1: Sub
    b.inst(OP_LABEL, &[op_lbls[1]]);
    let r = b.alloc_id();
    b.inst(OP_FSUB, &[ty_f32, r, va, vb]);
    finish_case(&mut b, r);

    // 2: Mul
    b.inst(OP_LABEL, &[op_lbls[2]]);
    let r = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, r, va, vb]);
    finish_case(&mut b, r);

    // 3: Div
    b.inst(OP_LABEL, &[op_lbls[3]]);
    let r = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, r, va, vb]);
    finish_case(&mut b, r);

    // 4: Pow
    b.inst(OP_LABEL, &[op_lbls[4]]);
    let r = emit_safe_pow(&mut b, va, vb);
    finish_case(&mut b, r);

    // 5: AddScalar
    b.inst(OP_LABEL, &[op_lbls[5]]);
    let r = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, r, va, imm]);
    finish_case(&mut b, r);

    // 6: SubScalar (a - s)
    b.inst(OP_LABEL, &[op_lbls[6]]);
    let r = b.alloc_id();
    b.inst(OP_FSUB, &[ty_f32, r, va, imm]);
    finish_case(&mut b, r);

    // 7: SubScalarRev (s - a)
    b.inst(OP_LABEL, &[op_lbls[7]]);
    let r = b.alloc_id();
    b.inst(OP_FSUB, &[ty_f32, r, imm, va]);
    finish_case(&mut b, r);

    // 8: MulScalar
    b.inst(OP_LABEL, &[op_lbls[8]]);
    let r = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, r, va, imm]);
    finish_case(&mut b, r);

    // 9: DivScalar (a / s)
    b.inst(OP_LABEL, &[op_lbls[9]]);
    let r = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, r, va, imm]);
    finish_case(&mut b, r);

    // 10: DivScalarRev (s / a)
    b.inst(OP_LABEL, &[op_lbls[10]]);
    let r = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, r, imm, va]);
    finish_case(&mut b, r);

    // 11: PowScalar (a ^ s)
    b.inst(OP_LABEL, &[op_lbls[11]]);
    let r = emit_safe_pow(&mut b, va, imm);
    finish_case(&mut b, r);

    // 12: PowScalarRev (s ^ a)
    b.inst(OP_LABEL, &[op_lbls[12]]);
    let r = emit_safe_pow(&mut b, imm, va);
    finish_case(&mut b, r);

    // 13: Neg
    b.inst(OP_LABEL, &[op_lbls[13]]);
    let r = b.alloc_id();
    b.inst(OP_FNEGATE, &[ty_f32, r, va]);
    finish_case(&mut b, r);

    // 14: Relu
    b.inst(OP_LABEL, &[op_lbls[14]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_FMAX, va, c_f0]);
    finish_case(&mut b, r);

    // 15: Gelu
    b.inst(OP_LABEL, &[op_lbls[15]]);
    let x2 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, x2, va, va]);
    let x3 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, x3, x2, va]);
    let cx3 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, cx3, c_gelu_b, x3]);
    let i1 = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, i1, va, cx3]);
    let i2 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, i2, c_gelu_a, i1]);
    let th = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, th, glsl_ext, GLSL_TANH, i2]);
    let op_th = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, op_th, c_f1, th]);
    let hx = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, hx, c_f05, va]);
    let r = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, r, hx, op_th]);
    finish_case(&mut b, r);

    // 16: Silu
    b.inst(OP_LABEL, &[op_lbls[16]]);
    let na = b.alloc_id();
    b.inst(OP_FNEGATE, &[ty_f32, na, va]);
    let nac = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, nac, glsl_ext, GLSL_FCLAMP, na, c_f_lo, c_f_hi],
    );
    let ena = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, ena, glsl_ext, GLSL_EXP, nac]);
    let ope = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, ope, c_f1, ena]);
    let r = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, r, va, ope]);
    finish_case(&mut b, r);

    // 17: Tanh
    b.inst(OP_LABEL, &[op_lbls[17]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_TANH, va]);
    finish_case(&mut b, r);

    // 18: Sigmoid
    b.inst(OP_LABEL, &[op_lbls[18]]);
    let na = b.alloc_id();
    b.inst(OP_FNEGATE, &[ty_f32, na, va]);
    let nac = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, nac, glsl_ext, GLSL_FCLAMP, na, c_f_lo, c_f_hi],
    );
    let ena = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, ena, glsl_ext, GLSL_EXP, nac]);
    let ope = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, ope, c_f1, ena]);
    let r = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, r, c_f1, ope]);
    finish_case(&mut b, r);

    // 19: Exp
    b.inst(OP_LABEL, &[op_lbls[19]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_EXP, va]);
    finish_case(&mut b, r);

    // 20: Ln
    b.inst(OP_LABEL, &[op_lbls[20]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_LOG, va]);
    finish_case(&mut b, r);

    // 21: Sqrt
    b.inst(OP_LABEL, &[op_lbls[21]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_SQRT, va]);
    finish_case(&mut b, r);

    // 22: Sin
    b.inst(OP_LABEL, &[op_lbls[22]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_SIN, va]);
    finish_case(&mut b, r);

    // 23: Cos
    b.inst(OP_LABEL, &[op_lbls[23]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_COS, va]);
    finish_case(&mut b, r);

    // 24: Abs (default)
    b.inst(OP_LABEL, &[op_lbls[24]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_FABS, va]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[lbl_step_merge]);
    let sres = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, sres, var_step_res]);
    let dst_slot = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, dst_slot, p_n_in, s_phi]);
    let p_dst = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, p_dst, var_slots, dst_slot]);
    b.inst(OP_STORE, &[p_dst, sres]);
    b.inst(OP_BRANCH, &[lbl_step_cont]);

    b.inst(OP_LABEL, &[lbl_step_cont]);
    b.inst(OP_IADD, &[ty_u32, s_next, s_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_step_hdr]);

    b.inst(OP_LABEL, &[lbl_step_exit]);
    let last_s0 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, last_s0, p_n_in, p_n_steps]);
    let last_slot = b.alloc_id();
    b.inst(OP_ISUB, &[ty_u32, last_slot, last_s0, c_u1]);
    let p_last = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, p_last, var_slots, last_slot]);
    let final_v = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, final_v, p_last]);
    b.store_buf(ptr_unif_f32, var_out, c_u0, tid, final_v);
    b.inst(OP_BRANCH, &[lbl_exit]);

    b.inst(OP_LABEL, &[lbl_exit]);
    b.inst(OP_RETURN, &[]);
    b.inst(OP_FUNCTION_END, &[]);
    b.finish()
}

// ═════════════════════════════════════════════════════════════════════════════
// G4: Shared Packed-16-bit (`f16`/`bf16`) SPIR-V 1.0 Helpers + 6 Native Half Shaders
// ═════════════════════════════════════════════════════════════════════════════

struct HalfSpirvCtx {
    glsl_ext: u32,
    ty_bool: u32,
    ty_u32: u32,
    ty_f32: u32,
    ty_vec2: u32,
    ptr_unif_u32: u32,
    c_u0: u32,
    c_u1: u32,
    c_u16: u32,
    c_mask_hi: u32,
    c_mask_lo: u32,
    c_mask_abs: u32,
    c_inf: u32,
    c_round_bias: u32,
    c_qnan: u32,
}

fn spv_unpack_half(
    b: &mut SpirvBuilder,
    hc: &HalfSpirvCtx,
    is_bf16: bool,
    var_buf: u32,
    idx: u32,
) -> u32 {
    let word_idx = b.alloc_id();
    b.inst(OP_SHIFT_RIGHT_LOGICAL, &[hc.ty_u32, word_idx, idx, hc.c_u1]);
    let word = b.load_buf(hc.ptr_unif_u32, hc.ty_u32, var_buf, hc.c_u0, word_idx);
    let lane_bit = b.alloc_id();
    b.inst(OP_BITWISE_AND, &[hc.ty_u32, lane_bit, idx, hc.c_u1]);
    let is_odd = b.alloc_id();
    b.inst(OP_INOT_EQUAL, &[hc.ty_bool, is_odd, lane_bit, hc.c_u0]);
    if is_bf16 {
        let lo_bits = b.alloc_id();
        b.inst(OP_SHIFT_LEFT_LOGICAL, &[hc.ty_u32, lo_bits, word, hc.c_u16]);
        let hi_bits = b.alloc_id();
        b.inst(OP_BITWISE_AND, &[hc.ty_u32, hi_bits, word, hc.c_mask_hi]);
        let bits = b.alloc_id();
        b.inst(OP_SELECT, &[hc.ty_u32, bits, is_odd, hi_bits, lo_bits]);
        let fval = b.alloc_id();
        b.inst(OP_BITCAST, &[hc.ty_f32, fval, bits]);
        fval
    } else {
        let pair = b.alloc_id();
        b.inst(
            OP_EXT_INST,
            &[hc.ty_vec2, pair, hc.glsl_ext, GLSL_UNPACK_HALF_2X16, word],
        );
        let f0 = b.alloc_id();
        b.inst(OP_COMPOSITE_EXTRACT, &[hc.ty_f32, f0, pair, 0]);
        let f1 = b.alloc_id();
        b.inst(OP_COMPOSITE_EXTRACT, &[hc.ty_f32, f1, pair, 1]);
        let fval = b.alloc_id();
        b.inst(OP_SELECT, &[hc.ty_f32, fval, is_odd, f1, f0]);
        fval
    }
}

fn spv_pack_half_pair(
    b: &mut SpirvBuilder,
    hc: &HalfSpirvCtx,
    is_bf16: bool,
    v0: u32,
    v1: u32,
) -> u32 {
    if is_bf16 {
        let pack_one = |b: &mut SpirvBuilder, fv: u32| -> u32 {
            let bits = b.alloc_id();
            b.inst(OP_BITCAST, &[hc.ty_u32, bits, fv]);
            let abs_b = b.alloc_id();
            b.inst(OP_BITWISE_AND, &[hc.ty_u32, abs_b, bits, hc.c_mask_abs]);
            let is_nan = b.alloc_id();
            b.inst(OP_UGREATER_THAN, &[hc.ty_bool, is_nan, abs_b, hc.c_inf]);
            let hi16 = b.alloc_id();
            b.inst(OP_SHIFT_RIGHT_LOGICAL, &[hc.ty_u32, hi16, bits, hc.c_u16]);
            let lsb = b.alloc_id();
            b.inst(OP_BITWISE_AND, &[hc.ty_u32, lsb, hi16, hc.c_u1]);
            let bias = b.alloc_id();
            b.inst(OP_IADD, &[hc.ty_u32, bias, hc.c_round_bias, lsb]);
            let rounded_raw = b.alloc_id();
            b.inst(OP_IADD, &[hc.ty_u32, rounded_raw, bits, bias]);
            let rounded = b.alloc_id();
            b.inst(
                OP_SHIFT_RIGHT_LOGICAL,
                &[hc.ty_u32, rounded, rounded_raw, hc.c_u16],
            );
            let nan_h = b.alloc_id();
            b.inst(OP_BITWISE_OR, &[hc.ty_u32, nan_h, hi16, hc.c_qnan]);
            let chosen = b.alloc_id();
            b.inst(OP_SELECT, &[hc.ty_u32, chosen, is_nan, nan_h, rounded]);
            let masked = b.alloc_id();
            b.inst(OP_BITWISE_AND, &[hc.ty_u32, masked, chosen, hc.c_mask_lo]);
            masked
        };
        let h0 = pack_one(b, v0);
        let h1 = pack_one(b, v1);
        let h1_sh = b.alloc_id();
        b.inst(OP_SHIFT_LEFT_LOGICAL, &[hc.ty_u32, h1_sh, h1, hc.c_u16]);
        let comb = b.alloc_id();
        b.inst(OP_BITWISE_OR, &[hc.ty_u32, comb, h0, h1_sh]);
        comb
    } else {
        let vec2_val = b.alloc_id();
        b.inst(OP_COMPOSITE_CONSTRUCT, &[hc.ty_vec2, vec2_val, v0, v1]);
        let pk = b.alloc_id();
        b.inst(
            OP_EXT_INST,
            &[hc.ty_u32, pk, hc.glsl_ext, GLSL_PACK_HALF_2X16, vec2_val],
        );
        pk
    }
}

/// Race-free packed 16-bit (`f16` or `bf16`) GPU RMSNorm (4 SSBOs, all `u32[]`).
/// Each thread computes 2 adjacent flat output elements (`pair_idx * 2 + lane`), eliminating all
/// write races even when `K` is odd.
pub(super) fn emit_spirv_rms_norm_half(is_bf16: bool) -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    b.inst(OP_CAPABILITY, &[1]);
    let glsl_ext = b.alloc_id();
    let mut ext_ops = vec![glsl_ext];
    SpirvBuilder::push_str_words(&mut ext_ops, "GLSL.std.450");
    b.inst(OP_EXT_INST_IMPORT, &ext_ops);
    b.inst(OP_MEMORY_MODEL, &[0, 1]);

    let main_fn = b.alloc_id();
    let var_gid = b.alloc_id();
    let mut ep_ops = vec![5, main_fn];
    SpirvBuilder::push_str_words(&mut ep_ops, "main");
    ep_ops.push(var_gid);
    b.inst(OP_ENTRY_POINT, &ep_ops);
    b.inst(OP_EXECUTION_MODE, &[main_fn, 17, 64, 1, 1]);

    let ty_u32_arr = b.alloc_id();
    let ty_u32_buf = b.alloc_id();
    let var_x = b.alloc_id();
    let var_gamma = b.alloc_id();
    let var_out = b.alloc_id();
    let var_params = b.alloc_id();

    b.inst(OP_DECORATE, &[var_gid, DEC_BUILT_IN, 28]);
    b.inst(OP_DECORATE, &[ty_u32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_u32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_u32_buf, 0, DEC_OFFSET, 0]);
    for (binding, var_id) in [(0u32, var_x), (1, var_gamma), (2, var_out), (3, var_params)] {
        b.inst(OP_DECORATE, &[var_id, DEC_DESCRIPTOR_SET, 0]);
        b.inst(OP_DECORATE, &[var_id, DEC_BINDING, binding]);
    }

    let ty_void = b.alloc_id();
    b.inst(OP_TYPE_VOID, &[ty_void]);
    let ty_fn = b.alloc_id();
    b.inst(OP_TYPE_FUNCTION, &[ty_fn, ty_void]);
    let ty_bool = b.alloc_id();
    b.inst(OP_TYPE_BOOL, &[ty_bool]);
    let ty_u32 = b.alloc_id();
    b.inst(OP_TYPE_INT, &[ty_u32, 32, 0]);
    let ty_f32 = b.alloc_id();
    b.inst(OP_TYPE_FLOAT, &[ty_f32, 32]);
    let ty_vec2 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_vec2, ty_f32, 2]);
    let ty_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_uvec3, ty_u32, 3]);
    let ptr_in_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_in_uvec3, SC_INPUT, ty_uvec3]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_u32_arr, ty_u32]);
    b.inst(OP_TYPE_STRUCT, &[ty_u32_buf, ty_u32_arr]);
    let ptr_unif_u32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32_buf, SC_UNIFORM, ty_u32_buf]);
    let ptr_unif_u32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32, SC_UNIFORM, ty_u32]);

    let c_u0 = b.c_u32(ty_u32, 0);
    let c_u1 = b.c_u32(ty_u32, 1);
    let c_u2 = b.c_u32(ty_u32, 2);
    let c_u3 = b.c_u32(ty_u32, 3);
    let c_u4 = b.c_u32(ty_u32, 4);
    let c_f0 = b.c_f32(ty_f32, 0.0);
    let c_f1 = b.c_f32(ty_f32, 1.0);
    let hc = HalfSpirvCtx {
        glsl_ext,
        ty_bool,
        ty_u32,
        ty_f32,
        ty_vec2,
        ptr_unif_u32,
        c_u0,
        c_u1,
        c_u16: b.c_u32(ty_u32, 16),
        c_mask_hi: b.c_u32(ty_u32, 0xffff_0000),
        c_mask_lo: b.c_u32(ty_u32, 0x0000_ffff),
        c_mask_abs: b.c_u32(ty_u32, 0x7fff_ffff),
        c_inf: b.c_u32(ty_u32, 0x7f80_0000),
        c_round_bias: b.c_u32(ty_u32, 0x0000_7fff),
        c_qnan: b.c_u32(ty_u32, 0x0000_0040),
    };

    let ty_arr2_f32 = b.alloc_id();
    b.inst(OP_TYPE_ARRAY, &[ty_arr2_f32, ty_f32, c_u2]);
    let ptr_fn_arr2 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_arr2, SC_FUNCTION, ty_arr2_f32]);
    let ptr_fn_f32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_f32, SC_FUNCTION, ty_f32]);

    b.inst(OP_VARIABLE, &[ptr_in_uvec3, var_gid, SC_INPUT]);
    for v in [var_x, var_gamma, var_out, var_params] {
        b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, v, SC_UNIFORM]);
    }

    b.inst(OP_FUNCTION, &[ty_void, main_fn, 0, ty_fn]);
    let lbl_entry = b.alloc_id();
    let lbl_body = b.alloc_id();
    let lbl_lane_hdr = b.alloc_id();
    let lbl_lane_body = b.alloc_id();
    let lbl_in_elem = b.alloc_id();
    let lbl_k_hdr = b.alloc_id();
    let lbl_k_body = b.alloc_id();
    let lbl_k_cont = b.alloc_id();
    let lbl_k_exit = b.alloc_id();
    let lbl_lane_merge = b.alloc_id();
    let lbl_lane_cont = b.alloc_id();
    let lbl_lane_exit = b.alloc_id();
    let lbl_exit = b.alloc_id();

    b.inst(OP_LABEL, &[lbl_entry]);
    let var_lane_out = b.alloc_id();
    b.inst(OP_VARIABLE, &[ptr_fn_arr2, var_lane_out, SC_FUNCTION]);

    let gid_val = b.alloc_id();
    b.inst(OP_LOAD, &[ty_uvec3, gid_val, var_gid]);
    let pair_idx = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, pair_idx, gid_val, 0]);

    let p_m = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u0);
    let p_k = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u1);
    let p_has_gamma = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u2);
    let p_gamma_len = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u3);
    let p_eps_bits = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u0, c_u4);
    let eps = b.alloc_id();
    b.inst(OP_BITCAST, &[ty_f32, eps, p_eps_bits]);
    let has_gamma_b = b.alloc_id();
    b.inst(OP_INOT_EQUAL, &[ty_bool, has_gamma_b, p_has_gamma, c_u0]);
    let k_f = b.alloc_id();
    b.inst(OP_CONVERT_U_TO_F, &[ty_f32, k_f, p_k]);

    let total_elems = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, total_elems, p_m, p_k]);
    let tp1 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, tp1, total_elems, c_u1]);
    let total_pairs = b.alloc_id();
    b.inst(OP_SHIFT_RIGHT_LOGICAL, &[ty_u32, total_pairs, tp1, c_u1]);

    let in_bounds = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, in_bounds, pair_idx, total_pairs]);
    b.inst(OP_SELECTION_MERGE, &[lbl_exit, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[in_bounds, lbl_body, lbl_exit]);

    b.inst(OP_LABEL, &[lbl_body]);
    let p_lo0 = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, p_lo0, var_lane_out, c_u0]);
    b.inst(OP_STORE, &[p_lo0, c_f0]);
    let p_lo1 = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, p_lo1, var_lane_out, c_u1]);
    b.inst(OP_STORE, &[p_lo1, c_f0]);
    let base_flat = b.alloc_id();
    b.inst(OP_SHIFT_LEFT_LOGICAL, &[ty_u32, base_flat, pair_idx, c_u1]);
    b.inst(OP_BRANCH, &[lbl_lane_hdr]);

    let lane_phi = b.alloc_id();
    let lane_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_lane_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, lane_phi, c_u0, lbl_body, lane_next, lbl_lane_cont],
    );
    let lane_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, lane_cond, lane_phi, c_u2]);
    b.inst(OP_LOOP_MERGE, &[lbl_lane_exit, lbl_lane_cont, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[lane_cond, lbl_lane_body, lbl_lane_exit],
    );

    b.inst(OP_LABEL, &[lbl_lane_body]);
    let flat = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, flat, base_flat, lane_phi]);
    let elem_ok = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, elem_ok, flat, total_elems]);
    b.inst(OP_SELECTION_MERGE, &[lbl_lane_merge, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[elem_ok, lbl_in_elem, lbl_lane_merge],
    );

    b.inst(OP_LABEL, &[lbl_in_elem]);
    let row = b.alloc_id();
    b.inst(OP_UDIV, &[ty_u32, row, flat, p_k]);
    let col = b.alloc_id();
    b.inst(OP_UMOD, &[ty_u32, col, flat, p_k]);
    let row_k = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, row_k, row, p_k]);
    b.inst(OP_BRANCH, &[lbl_k_hdr]);

    let t_phi = b.alloc_id();
    let sq_phi = b.alloc_id();
    let t_next = b.alloc_id();
    let sq_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_k_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, t_phi, c_u0, lbl_in_elem, t_next, lbl_k_cont],
    );
    b.inst(
        OP_PHI,
        &[ty_f32, sq_phi, c_f0, lbl_in_elem, sq_next, lbl_k_cont],
    );
    let k_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, k_cond, t_phi, p_k]);
    b.inst(OP_LOOP_MERGE, &[lbl_k_exit, lbl_k_cont, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[k_cond, lbl_k_body, lbl_k_exit]);

    b.inst(OP_LABEL, &[lbl_k_body]);
    let x_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, x_idx, row_k, t_phi]);
    let xv = spv_unpack_half(&mut b, &hc, is_bf16, var_x, x_idx);
    let xv_sq = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, xv_sq, xv, xv]);
    b.inst(OP_FADD, &[ty_f32, sq_next, sq_phi, xv_sq]);
    b.inst(OP_BRANCH, &[lbl_k_cont]);

    b.inst(OP_LABEL, &[lbl_k_cont]);
    b.inst(OP_IADD, &[ty_u32, t_next, t_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_k_hdr]);

    b.inst(OP_LABEL, &[lbl_k_exit]);
    let ms = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, ms, sq_phi, k_f]);
    let ms_eps = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, ms_eps, ms, eps]);
    let rms = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, rms, glsl_ext, GLSL_SQRT, ms_eps]);
    let inv_rms = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, inv_rms, c_f1, rms]);
    let cur_x = spv_unpack_half(&mut b, &hc, is_bf16, var_x, flat);
    let xn = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, xn, cur_x, inv_rms]);
    let g_idx = b.alloc_id();
    b.inst(OP_UMOD, &[ty_u32, g_idx, col, p_gamma_len]);
    let g_raw = spv_unpack_half(&mut b, &hc, is_bf16, var_gamma, g_idx);
    let g_eff = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, g_eff, has_gamma_b, g_raw, c_f1]);
    let out_v = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, out_v, xn, g_eff]);
    let p_dst = b.alloc_id();
    b.inst(
        OP_ACCESS_CHAIN,
        &[ptr_fn_f32, p_dst, var_lane_out, lane_phi],
    );
    b.inst(OP_STORE, &[p_dst, out_v]);
    b.inst(OP_BRANCH, &[lbl_lane_merge]);

    b.inst(OP_LABEL, &[lbl_lane_merge]);
    b.inst(OP_BRANCH, &[lbl_lane_cont]);

    b.inst(OP_LABEL, &[lbl_lane_cont]);
    b.inst(OP_IADD, &[ty_u32, lane_next, lane_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_lane_hdr]);

    b.inst(OP_LABEL, &[lbl_lane_exit]);
    let v0 = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, v0, p_lo0]);
    let v1 = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, v1, p_lo1]);
    let packed = spv_pack_half_pair(&mut b, &hc, is_bf16, v0, v1);
    b.store_buf(ptr_unif_u32, var_out, c_u0, pair_idx, packed);
    b.inst(OP_BRANCH, &[lbl_exit]);

    b.inst(OP_LABEL, &[lbl_exit]);
    b.inst(OP_RETURN, &[]);
    b.inst(OP_FUNCTION_END, &[]);
    b.finish()
}

/// Race-free packed 16-bit (`f16` or `bf16`) single-pass Online-Softmax FlashAttention-2
/// (10 SSBOs, all `u32[]`). Each thread computes 2 adjacent flat output elements.
pub(super) fn emit_spirv_flash_attention_half(is_bf16: bool) -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    b.inst(OP_CAPABILITY, &[1]);
    let glsl_ext = b.alloc_id();
    let mut ext_ops = vec![glsl_ext];
    SpirvBuilder::push_str_words(&mut ext_ops, "GLSL.std.450");
    b.inst(OP_EXT_INST_IMPORT, &ext_ops);
    b.inst(OP_MEMORY_MODEL, &[0, 1]);

    let main_fn = b.alloc_id();
    let var_gid = b.alloc_id();
    let mut ep_ops = vec![5, main_fn];
    SpirvBuilder::push_str_words(&mut ep_ops, "main");
    ep_ops.push(var_gid);
    b.inst(OP_ENTRY_POINT, &ep_ops);
    b.inst(OP_EXECUTION_MODE, &[main_fn, 17, 64, 1, 1]);

    let ty_u32_arr = b.alloc_id();
    let ty_u32_buf = b.alloc_id();
    let var_q = b.alloc_id();
    let var_k = b.alloc_id();
    let var_v = b.alloc_id();
    let var_mask = b.alloc_id();
    let var_out = b.alloc_id();
    let var_qb = b.alloc_id();
    let var_kb = b.alloc_id();
    let var_vb = b.alloc_id();
    let var_mb = b.alloc_id();
    let var_params = b.alloc_id();

    b.inst(OP_DECORATE, &[var_gid, DEC_BUILT_IN, 28]);
    b.inst(OP_DECORATE, &[ty_u32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_u32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_u32_buf, 0, DEC_OFFSET, 0]);
    for (binding, var_id) in [
        (0u32, var_q),
        (1, var_k),
        (2, var_v),
        (3, var_mask),
        (4, var_out),
        (5, var_qb),
        (6, var_kb),
        (7, var_vb),
        (8, var_mb),
        (9, var_params),
    ] {
        b.inst(OP_DECORATE, &[var_id, DEC_DESCRIPTOR_SET, 0]);
        b.inst(OP_DECORATE, &[var_id, DEC_BINDING, binding]);
    }

    let ty_void = b.alloc_id();
    b.inst(OP_TYPE_VOID, &[ty_void]);
    let ty_fn = b.alloc_id();
    b.inst(OP_TYPE_FUNCTION, &[ty_fn, ty_void]);
    let ty_bool = b.alloc_id();
    b.inst(OP_TYPE_BOOL, &[ty_bool]);
    let ty_u32 = b.alloc_id();
    b.inst(OP_TYPE_INT, &[ty_u32, 32, 0]);
    let ty_f32 = b.alloc_id();
    b.inst(OP_TYPE_FLOAT, &[ty_f32, 32]);
    let ty_vec2 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_vec2, ty_f32, 2]);
    let ty_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_uvec3, ty_u32, 3]);
    let ptr_in_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_in_uvec3, SC_INPUT, ty_uvec3]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_u32_arr, ty_u32]);
    b.inst(OP_TYPE_STRUCT, &[ty_u32_buf, ty_u32_arr]);
    let ptr_unif_u32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32_buf, SC_UNIFORM, ty_u32_buf]);
    let ptr_unif_u32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32, SC_UNIFORM, ty_u32]);

    let c_u: Vec<u32> = (0..10).map(|i| b.c_u32(ty_u32, i)).collect();
    let c_f0 = b.c_f32(ty_f32, 0.0);
    let c_f_neginf = b.c_f32(ty_f32, -3.4e38_f32);
    let c_f_mask_cut = b.c_f32(ty_f32, -1.0e30_f32);
    let c_f_exp_lo = b.c_f32(ty_f32, -80.0);
    let hc = HalfSpirvCtx {
        glsl_ext,
        ty_bool,
        ty_u32,
        ty_f32,
        ty_vec2,
        ptr_unif_u32,
        c_u0: c_u[0],
        c_u1: c_u[1],
        c_u16: b.c_u32(ty_u32, 16),
        c_mask_hi: b.c_u32(ty_u32, 0xffff_0000),
        c_mask_lo: b.c_u32(ty_u32, 0x0000_ffff),
        c_mask_abs: b.c_u32(ty_u32, 0x7fff_ffff),
        c_inf: b.c_u32(ty_u32, 0x7f80_0000),
        c_round_bias: b.c_u32(ty_u32, 0x0000_7fff),
        c_qnan: b.c_u32(ty_u32, 0x0000_0040),
    };

    let ty_arr2_f32 = b.alloc_id();
    b.inst(OP_TYPE_ARRAY, &[ty_arr2_f32, ty_f32, c_u[2]]);
    let ptr_fn_arr2 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_arr2, SC_FUNCTION, ty_arr2_f32]);
    let ptr_fn_f32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_f32, SC_FUNCTION, ty_f32]);

    b.inst(OP_VARIABLE, &[ptr_in_uvec3, var_gid, SC_INPUT]);
    for v in [
        var_q, var_k, var_v, var_mask, var_out, var_qb, var_kb, var_vb, var_mb, var_params,
    ] {
        b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, v, SC_UNIFORM]);
    }

    b.inst(OP_FUNCTION, &[ty_void, main_fn, 0, ty_fn]);
    let lbl_entry = b.alloc_id();
    let lbl_body = b.alloc_id();
    let lbl_lane_hdr = b.alloc_id();
    let lbl_lane_body = b.alloc_id();
    let lbl_in_elem = b.alloc_id();
    let lbl_k_hdr = b.alloc_id();
    let lbl_k_body = b.alloc_id();
    let lbl_d_hdr = b.alloc_id();
    let lbl_d_body = b.alloc_id();
    let lbl_d_cont = b.alloc_id();
    let lbl_d_exit = b.alloc_id();
    let lbl_k_cont = b.alloc_id();
    let lbl_k_exit = b.alloc_id();
    let lbl_lane_merge = b.alloc_id();
    let lbl_lane_cont = b.alloc_id();
    let lbl_lane_exit = b.alloc_id();
    let lbl_exit = b.alloc_id();

    b.inst(OP_LABEL, &[lbl_entry]);
    let var_lane_out = b.alloc_id();
    b.inst(OP_VARIABLE, &[ptr_fn_arr2, var_lane_out, SC_FUNCTION]);

    let gid_val = b.alloc_id();
    b.inst(OP_LOAD, &[ty_uvec3, gid_val, var_gid]);
    let pair_idx = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, pair_idx, gid_val, 0]);

    let p_batches = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[0]);
    let p_sq = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[1]);
    let p_sk = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[2]);
    let p_dk = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[3]);
    let p_dv = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[4]);
    let p_kt = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[5]);
    let p_hm = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[6]);
    let p_msq = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[7]);
    let p_msk = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[8]);
    let p_scale_bits = b.load_buf(ptr_unif_u32, ty_u32, var_params, c_u[0], c_u[9]);
    let scale = b.alloc_id();
    b.inst(OP_BITCAST, &[ty_f32, scale, p_scale_bits]);
    let is_kt = b.alloc_id();
    b.inst(OP_INOT_EQUAL, &[ty_bool, is_kt, p_kt, c_u[0]]);
    let has_mask_b = b.alloc_id();
    b.inst(OP_INOT_EQUAL, &[ty_bool, has_mask_b, p_hm, c_u[0]]);

    let sq_dv = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, sq_dv, p_sq, p_dv]);
    let total_elems = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, total_elems, p_batches, sq_dv]);
    let tp1 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, tp1, total_elems, c_u[1]]);
    let total_pairs = b.alloc_id();
    b.inst(OP_SHIFT_RIGHT_LOGICAL, &[ty_u32, total_pairs, tp1, c_u[1]]);

    let in_bounds = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, in_bounds, pair_idx, total_pairs]);
    b.inst(OP_SELECTION_MERGE, &[lbl_exit, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[in_bounds, lbl_body, lbl_exit]);

    b.inst(OP_LABEL, &[lbl_body]);
    let p_lo0 = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, p_lo0, var_lane_out, c_u[0]]);
    b.inst(OP_STORE, &[p_lo0, c_f0]);
    let p_lo1 = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, p_lo1, var_lane_out, c_u[1]]);
    b.inst(OP_STORE, &[p_lo1, c_f0]);
    let base_flat = b.alloc_id();
    b.inst(
        OP_SHIFT_LEFT_LOGICAL,
        &[ty_u32, base_flat, pair_idx, c_u[1]],
    );
    b.inst(OP_BRANCH, &[lbl_lane_hdr]);

    let lane_phi = b.alloc_id();
    let lane_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_lane_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, lane_phi, c_u[0], lbl_body, lane_next, lbl_lane_cont],
    );
    let lane_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, lane_cond, lane_phi, c_u[2]]);
    b.inst(OP_LOOP_MERGE, &[lbl_lane_exit, lbl_lane_cont, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[lane_cond, lbl_lane_body, lbl_lane_exit],
    );

    b.inst(OP_LABEL, &[lbl_lane_body]);
    let flat = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, flat, base_flat, lane_phi]);
    let elem_ok = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, elem_ok, flat, total_elems]);
    b.inst(OP_SELECTION_MERGE, &[lbl_lane_merge, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[elem_ok, lbl_in_elem, lbl_lane_merge],
    );

    b.inst(OP_LABEL, &[lbl_in_elem]);
    let batch = b.alloc_id();
    b.inst(OP_UDIV, &[ty_u32, batch, flat, sq_dv]);
    let rem = b.alloc_id();
    b.inst(OP_UMOD, &[ty_u32, rem, flat, sq_dv]);
    let qi = b.alloc_id();
    b.inst(OP_UDIV, &[ty_u32, qi, rem, p_dv]);
    let di = b.alloc_id();
    b.inst(OP_UMOD, &[ty_u32, di, rem, p_dv]);

    let q_base0 = b.load_buf(ptr_unif_u32, ty_u32, var_qb, c_u[0], batch);
    let k_base = b.load_buf(ptr_unif_u32, ty_u32, var_kb, c_u[0], batch);
    let v_base = b.load_buf(ptr_unif_u32, ty_u32, var_vb, c_u[0], batch);
    let m_base = b.load_buf(ptr_unif_u32, ty_u32, var_mb, c_u[0], batch);
    let qi_dk = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, qi_dk, qi, p_dk]);
    let q_row_base = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, q_row_base, q_base0, qi_dk]);
    let qi_msq = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, qi_msq, qi, p_msq]);
    let mask_row_base = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, mask_row_base, m_base, qi_msq]);
    b.inst(OP_BRANCH, &[lbl_k_hdr]);

    let kj_phi = b.alloc_id();
    let m_phi = b.alloc_id();
    let l_phi = b.alloc_id();
    let acc_phi = b.alloc_id();
    let kj_next = b.alloc_id();
    let m_next = b.alloc_id();
    let l_next = b.alloc_id();
    let acc_next = b.alloc_id();

    b.inst(OP_LABEL, &[lbl_k_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, kj_phi, c_u[0], lbl_in_elem, kj_next, lbl_k_cont],
    );
    b.inst(
        OP_PHI,
        &[ty_f32, m_phi, c_f_neginf, lbl_in_elem, m_next, lbl_k_cont],
    );
    b.inst(
        OP_PHI,
        &[ty_f32, l_phi, c_f0, lbl_in_elem, l_next, lbl_k_cont],
    );
    b.inst(
        OP_PHI,
        &[ty_f32, acc_phi, c_f0, lbl_in_elem, acc_next, lbl_k_cont],
    );
    let k_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, k_cond, kj_phi, p_sk]);
    b.inst(OP_LOOP_MERGE, &[lbl_k_exit, lbl_k_cont, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[k_cond, lbl_k_body, lbl_k_exit]);

    b.inst(OP_LABEL, &[lbl_k_body]);
    let kj_dk = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, kj_dk, kj_phi, p_dk]);
    let k_norm_row = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, k_norm_row, k_base, kj_dk]);
    b.inst(OP_BRANCH, &[lbl_d_hdr]);

    let t_phi = b.alloc_id();
    let dot_phi = b.alloc_id();
    let t_next = b.alloc_id();
    let dot_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_d_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, t_phi, c_u[0], lbl_k_body, t_next, lbl_d_cont],
    );
    b.inst(
        OP_PHI,
        &[ty_f32, dot_phi, c_f0, lbl_k_body, dot_next, lbl_d_cont],
    );
    let d_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, d_cond, t_phi, p_dk]);
    b.inst(OP_LOOP_MERGE, &[lbl_d_exit, lbl_d_cont, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[d_cond, lbl_d_body, lbl_d_exit]);

    b.inst(OP_LABEL, &[lbl_d_body]);
    let q_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, q_idx, q_row_base, t_phi]);
    let qv = spv_unpack_half(&mut b, &hc, is_bf16, var_q, q_idx);

    let k_idx_norm = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, k_idx_norm, k_norm_row, t_phi]);
    let t_sk = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, t_sk, t_phi, p_sk]);
    let k_t0 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, k_t0, k_base, t_sk]);
    let k_idx_tr = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, k_idx_tr, k_t0, kj_phi]);
    let k_idx = b.alloc_id();
    b.inst(OP_SELECT, &[ty_u32, k_idx, is_kt, k_idx_tr, k_idx_norm]);
    let kv = spv_unpack_half(&mut b, &hc, is_bf16, var_k, k_idx);

    let prod = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, prod, qv, kv]);
    b.inst(OP_FADD, &[ty_f32, dot_next, dot_phi, prod]);
    b.inst(OP_BRANCH, &[lbl_d_cont]);

    b.inst(OP_LABEL, &[lbl_d_cont]);
    b.inst(OP_IADD, &[ty_u32, t_next, t_phi, c_u[1]]);
    b.inst(OP_BRANCH, &[lbl_d_hdr]);

    b.inst(OP_LABEL, &[lbl_d_exit]);
    let score0 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, score0, dot_phi, scale]);

    let kj_msk = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, kj_msk, kj_phi, p_msk]);
    let mask_idx_raw = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, mask_idx_raw, mask_row_base, kj_msk]);
    let mask_idx = b.alloc_id();
    b.inst(
        OP_SELECT,
        &[ty_u32, mask_idx, has_mask_b, mask_idx_raw, c_u[0]],
    );
    let mask_v = spv_unpack_half(&mut b, &hc, is_bf16, var_mask, mask_idx);
    let score_m = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, score_m, score0, mask_v]);
    let score = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, score, has_mask_b, score_m, score0]);

    let valid = b.alloc_id();
    b.inst(OP_FORD_GREATER_THAN, &[ty_bool, valid, score, c_f_mask_cut]);
    let eff_score = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, eff_score, valid, score, m_phi]);
    let m_cand = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, m_cand, glsl_ext, GLSL_FMAX, m_phi, eff_score],
    );
    b.inst(OP_SELECT, &[ty_f32, m_next, valid, m_cand, m_phi]);

    let l_zero = b.alloc_id();
    b.inst(OP_FORD_EQUAL, &[ty_bool, l_zero, l_phi, c_f0]);
    let dm = b.alloc_id();
    b.inst(OP_FSUB, &[ty_f32, dm, m_phi, m_next]);
    let dm_c = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, dm_c, glsl_ext, GLSL_FCLAMP, dm, c_f_exp_lo, c_f0],
    );
    let exp_dm = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, exp_dm, glsl_ext, GLSL_EXP, dm_c]);
    let alpha = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, alpha, l_zero, c_f0, exp_dm]);

    let ds = b.alloc_id();
    b.inst(OP_FSUB, &[ty_f32, ds, eff_score, m_next]);
    let ds_c = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, ds_c, glsl_ext, GLSL_FCLAMP, ds, c_f_exp_lo, c_f0],
    );
    let exp_ds = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, exp_ds, glsl_ext, GLSL_EXP, ds_c]);
    let p_w = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, p_w, valid, exp_ds, c_f0]);

    let kj_dv = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, kj_dv, kj_phi, p_dv]);
    let v_i0 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, v_i0, v_base, kj_dv]);
    let v_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, v_idx, v_i0, di]);
    let vv = spv_unpack_half(&mut b, &hc, is_bf16, var_v, v_idx);

    let l_scaled = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, l_scaled, l_phi, alpha]);
    let l_cand = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, l_cand, l_scaled, p_w]);
    b.inst(OP_SELECT, &[ty_f32, l_next, valid, l_cand, l_phi]);

    let acc_scaled = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, acc_scaled, acc_phi, alpha]);
    let pv = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, pv, p_w, vv]);
    let acc_cand = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, acc_cand, acc_scaled, pv]);
    b.inst(OP_SELECT, &[ty_f32, acc_next, valid, acc_cand, acc_phi]);
    b.inst(OP_BRANCH, &[lbl_k_cont]);

    b.inst(OP_LABEL, &[lbl_k_cont]);
    b.inst(OP_IADD, &[ty_u32, kj_next, kj_phi, c_u[1]]);
    b.inst(OP_BRANCH, &[lbl_k_hdr]);

    b.inst(OP_LABEL, &[lbl_k_exit]);
    let l_pos = b.alloc_id();
    b.inst(OP_FORD_GREATER_THAN, &[ty_bool, l_pos, l_phi, c_f0]);
    let acc_div = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, acc_div, acc_phi, l_phi]);
    let out_val = b.alloc_id();
    b.inst(OP_SELECT, &[ty_f32, out_val, l_pos, acc_div, c_f0]);
    let p_dst = b.alloc_id();
    b.inst(
        OP_ACCESS_CHAIN,
        &[ptr_fn_f32, p_dst, var_lane_out, lane_phi],
    );
    b.inst(OP_STORE, &[p_dst, out_val]);
    b.inst(OP_BRANCH, &[lbl_lane_merge]);

    b.inst(OP_LABEL, &[lbl_lane_merge]);
    b.inst(OP_BRANCH, &[lbl_lane_cont]);

    b.inst(OP_LABEL, &[lbl_lane_cont]);
    b.inst(OP_IADD, &[ty_u32, lane_next, lane_phi, c_u[1]]);
    b.inst(OP_BRANCH, &[lbl_lane_hdr]);

    b.inst(OP_LABEL, &[lbl_lane_exit]);
    let v0 = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, v0, p_lo0]);
    let v1 = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, v1, p_lo1]);
    let packed = spv_pack_half_pair(&mut b, &hc, is_bf16, v0, v1);
    b.store_buf(ptr_unif_u32, var_out, c_u[0], pair_idx, packed);
    b.inst(OP_BRANCH, &[lbl_exit]);

    b.inst(OP_LABEL, &[lbl_exit]);
    b.inst(OP_RETURN, &[]);
    b.inst(OP_FUNCTION_END, &[]);
    b.finish()
}

/// Race-free packed 16-bit (`f16` or `bf16`) 25-opcode FusedElementwise broadcast evaluator
/// (11 SSBOs, all `u32[]`). Each thread evaluates 2 adjacent flat output elements in `f32`
/// registers and packs them race-free into `Out[pair_idx]`.
pub(super) fn emit_spirv_fused_elementwise_half(is_bf16: bool) -> Vec<u32> {
    let mut b = SpirvBuilder::new();
    b.inst(OP_CAPABILITY, &[1]);
    let glsl_ext = b.alloc_id();
    let mut ext_ops = vec![glsl_ext];
    SpirvBuilder::push_str_words(&mut ext_ops, "GLSL.std.450");
    b.inst(OP_EXT_INST_IMPORT, &ext_ops);
    b.inst(OP_MEMORY_MODEL, &[0, 1]);

    let main_fn = b.alloc_id();
    let var_gid = b.alloc_id();
    let mut ep_ops = vec![5, main_fn];
    SpirvBuilder::push_str_words(&mut ep_ops, "main");
    ep_ops.push(var_gid);
    b.inst(OP_ENTRY_POINT, &ep_ops);
    b.inst(OP_EXECUTION_MODE, &[main_fn, 17, 64, 1, 1]);

    let ty_u32_arr = b.alloc_id();
    let ty_u32_buf = b.alloc_id();
    let var_out = b.alloc_id();
    let var_in0 = b.alloc_id();
    let var_in1 = b.alloc_id();
    let var_in2 = b.alloc_id();
    let var_in3 = b.alloc_id();
    let var_strides = b.alloc_id();
    let var_oshape = b.alloc_id();
    let var_ops = b.alloc_id();
    let var_arga = b.alloc_id();
    let var_argb = b.alloc_id();
    let var_meta = b.alloc_id();

    b.inst(OP_DECORATE, &[var_gid, DEC_BUILT_IN, 28]);
    b.inst(OP_DECORATE, &[ty_u32_arr, DEC_ARRAY_STRIDE, 4]);
    b.inst(OP_DECORATE, &[ty_u32_buf, DEC_BUFFER_BLOCK]);
    b.inst(OP_MEMBER_DECORATE, &[ty_u32_buf, 0, DEC_OFFSET, 0]);
    for (binding, var_id) in [
        (0u32, var_out),
        (1, var_in0),
        (2, var_in1),
        (3, var_in2),
        (4, var_in3),
        (5, var_strides),
        (6, var_oshape),
        (7, var_ops),
        (8, var_arga),
        (9, var_argb),
        (10, var_meta),
    ] {
        b.inst(OP_DECORATE, &[var_id, DEC_DESCRIPTOR_SET, 0]);
        b.inst(OP_DECORATE, &[var_id, DEC_BINDING, binding]);
    }

    let ty_void = b.alloc_id();
    b.inst(OP_TYPE_VOID, &[ty_void]);
    let ty_fn = b.alloc_id();
    b.inst(OP_TYPE_FUNCTION, &[ty_fn, ty_void]);
    let ty_bool = b.alloc_id();
    b.inst(OP_TYPE_BOOL, &[ty_bool]);
    let ty_u32 = b.alloc_id();
    b.inst(OP_TYPE_INT, &[ty_u32, 32, 0]);
    let ty_f32 = b.alloc_id();
    b.inst(OP_TYPE_FLOAT, &[ty_f32, 32]);
    let ty_vec2 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_vec2, ty_f32, 2]);
    let ty_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_VECTOR, &[ty_uvec3, ty_u32, 3]);
    let ptr_in_uvec3 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_in_uvec3, SC_INPUT, ty_uvec3]);

    b.inst(OP_TYPE_RUNTIME_ARRAY, &[ty_u32_arr, ty_u32]);
    b.inst(OP_TYPE_STRUCT, &[ty_u32_buf, ty_u32_arr]);
    let ptr_unif_u32_buf = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32_buf, SC_UNIFORM, ty_u32_buf]);
    let ptr_unif_u32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_unif_u32, SC_UNIFORM, ty_u32]);

    let c_u0 = b.c_u32(ty_u32, 0);
    let c_u1 = b.c_u32(ty_u32, 1);
    let c_u2 = b.c_u32(ty_u32, 2);
    let c_u3 = b.c_u32(ty_u32, 3);
    let c_u4 = b.c_u32(ty_u32, 4);
    let c_u5 = b.c_u32(ty_u32, 5);
    let c_u16 = b.c_u32(ty_u32, 16);
    let c_u64 = b.c_u32(ty_u32, 64);

    let hc = HalfSpirvCtx {
        glsl_ext,
        ty_bool,
        ty_u32,
        ty_f32,
        ty_vec2,
        ptr_unif_u32,
        c_u0,
        c_u1,
        c_u16,
        c_mask_hi: b.c_u32(ty_u32, 0xffff_0000),
        c_mask_lo: b.c_u32(ty_u32, 0x0000_ffff),
        c_mask_abs: b.c_u32(ty_u32, 0x7fff_ffff),
        c_inf: b.c_u32(ty_u32, 0x7f80_0000),
        c_round_bias: b.c_u32(ty_u32, 0x0000_7fff),
        c_qnan: b.c_u32(ty_u32, 0x0000_0040),
    };

    let ty_slots = b.alloc_id();
    b.inst(OP_TYPE_ARRAY, &[ty_slots, ty_f32, c_u64]);
    let ptr_fn_slots = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_slots, SC_FUNCTION, ty_slots]);
    let ty_coords = b.alloc_id();
    b.inst(OP_TYPE_ARRAY, &[ty_coords, ty_u32, c_u16]);
    let ptr_fn_coords = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_coords, SC_FUNCTION, ty_coords]);
    let ty_arr2_f32 = b.alloc_id();
    b.inst(OP_TYPE_ARRAY, &[ty_arr2_f32, ty_f32, c_u2]);
    let ptr_fn_arr2 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_arr2, SC_FUNCTION, ty_arr2_f32]);
    let ptr_fn_f32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_f32, SC_FUNCTION, ty_f32]);
    let ptr_fn_u32 = b.alloc_id();
    b.inst(OP_TYPE_POINTER, &[ptr_fn_u32, SC_FUNCTION, ty_u32]);

    let c_f0 = b.c_f32(ty_f32, 0.0);
    let c_f1 = b.c_f32(ty_f32, 1.0);
    let c_f2 = b.c_f32(ty_f32, 2.0);
    let c_f3 = b.c_f32(ty_f32, 3.0);
    let c_f05 = b.c_f32(ty_f32, 0.5);
    let c_gelu_a = b.c_f32(ty_f32, 0.797_884_6);
    let c_gelu_b = b.c_f32(ty_f32, 0.044_715);
    let c_f_lo = b.c_f32(ty_f32, -40.0);
    let c_f_hi = b.c_f32(ty_f32, 40.0);

    b.inst(OP_VARIABLE, &[ptr_in_uvec3, var_gid, SC_INPUT]);
    for v in [
        var_out,
        var_in0,
        var_in1,
        var_in2,
        var_in3,
        var_strides,
        var_oshape,
        var_ops,
        var_arga,
        var_argb,
        var_meta,
    ] {
        b.inst(OP_VARIABLE, &[ptr_unif_u32_buf, v, SC_UNIFORM]);
    }

    b.inst(OP_FUNCTION, &[ty_void, main_fn, 0, ty_fn]);
    let lbl_entry = b.alloc_id();
    let lbl_body = b.alloc_id();
    let lbl_lane_hdr = b.alloc_id();
    let lbl_lane_body = b.alloc_id();
    let lbl_in_elem = b.alloc_id();
    let lbl_dec_hdr = b.alloc_id();
    let lbl_dec_body = b.alloc_id();
    let lbl_dec_cont = b.alloc_id();
    let lbl_dec_exit = b.alloc_id();
    let lbl_in_hdr = b.alloc_id();
    let lbl_in_body = b.alloc_id();
    let lbl_st_hdr = b.alloc_id();
    let lbl_st_body = b.alloc_id();
    let lbl_st_cont = b.alloc_id();
    let lbl_st_exit = b.alloc_id();
    let lbl_in_packed = b.alloc_id();
    let lbl_in_direct = b.alloc_id();
    let lbl_in_c0 = b.alloc_id();
    let lbl_in_c1 = b.alloc_id();
    let lbl_in_c2 = b.alloc_id();
    let lbl_in_c3 = b.alloc_id();
    let lbl_in_sw_merge = b.alloc_id();
    let lbl_in_fetch_merge = b.alloc_id();
    let lbl_in_cont = b.alloc_id();
    let lbl_in_exit = b.alloc_id();
    let lbl_step_hdr = b.alloc_id();
    let lbl_step_body = b.alloc_id();
    let lbl_step_merge = b.alloc_id();
    let lbl_step_cont = b.alloc_id();
    let lbl_step_exit = b.alloc_id();
    let lbl_lane_merge = b.alloc_id();
    let lbl_lane_cont = b.alloc_id();
    let lbl_lane_exit = b.alloc_id();
    let lbl_exit = b.alloc_id();
    let op_lbls: Vec<u32> = (0..25).map(|_| b.alloc_id()).collect();

    b.inst(OP_LABEL, &[lbl_entry]);
    let var_slots = b.alloc_id();
    b.inst(OP_VARIABLE, &[ptr_fn_slots, var_slots, SC_FUNCTION]);
    let var_coords = b.alloc_id();
    b.inst(OP_VARIABLE, &[ptr_fn_coords, var_coords, SC_FUNCTION]);
    let var_lane_out = b.alloc_id();
    b.inst(OP_VARIABLE, &[ptr_fn_arr2, var_lane_out, SC_FUNCTION]);
    let var_in_val = b.alloc_id();
    b.inst(OP_VARIABLE, &[ptr_fn_f32, var_in_val, SC_FUNCTION]);
    let var_step_res = b.alloc_id();
    b.inst(OP_VARIABLE, &[ptr_fn_f32, var_step_res, SC_FUNCTION]);

    let gid_val = b.alloc_id();
    b.inst(OP_LOAD, &[ty_uvec3, gid_val, var_gid]);
    let pair_idx = b.alloc_id();
    b.inst(OP_COMPOSITE_EXTRACT, &[ty_u32, pair_idx, gid_val, 0]);

    let p_ndim = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, c_u0);
    let p_n_in = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, c_u1);
    let p_n_steps = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, c_u2);
    let p_numel = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, c_u3);
    let p_packed = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, c_u4);
    let is_packed = b.alloc_id();
    b.inst(OP_INOT_EQUAL, &[ty_bool, is_packed, p_packed, c_u0]);
    let offsets_base = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, offsets_base, c_u5, p_n_steps]);

    let tp1 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, tp1, p_numel, c_u1]);
    let total_pairs = b.alloc_id();
    b.inst(OP_SHIFT_RIGHT_LOGICAL, &[ty_u32, total_pairs, tp1, c_u1]);

    let in_bounds = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, in_bounds, pair_idx, total_pairs]);
    b.inst(OP_SELECTION_MERGE, &[lbl_exit, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[in_bounds, lbl_body, lbl_exit]);

    b.inst(OP_LABEL, &[lbl_body]);
    let p_lo0 = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, p_lo0, var_lane_out, c_u0]);
    b.inst(OP_STORE, &[p_lo0, c_f0]);
    let p_lo1 = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, p_lo1, var_lane_out, c_u1]);
    b.inst(OP_STORE, &[p_lo1, c_f0]);
    let base_flat = b.alloc_id();
    b.inst(OP_SHIFT_LEFT_LOGICAL, &[ty_u32, base_flat, pair_idx, c_u1]);
    b.inst(OP_BRANCH, &[lbl_lane_hdr]);

    let lane_phi = b.alloc_id();
    let lane_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_lane_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, lane_phi, c_u0, lbl_body, lane_next, lbl_lane_cont],
    );
    let lane_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, lane_cond, lane_phi, c_u2]);
    b.inst(OP_LOOP_MERGE, &[lbl_lane_exit, lbl_lane_cont, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[lane_cond, lbl_lane_body, lbl_lane_exit],
    );

    b.inst(OP_LABEL, &[lbl_lane_body]);
    let tid = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, tid, base_flat, lane_phi]);
    let elem_ok = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, elem_ok, tid, p_numel]);
    b.inst(OP_SELECTION_MERGE, &[lbl_lane_merge, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[elem_ok, lbl_in_elem, lbl_lane_merge],
    );

    b.inst(OP_LABEL, &[lbl_in_elem]);
    b.inst(OP_BRANCH, &[lbl_dec_hdr]);

    let d_rem_phi = b.alloc_id();
    let rem_phi = b.alloc_id();
    let d_rem_next = b.alloc_id();
    let rem_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_dec_hdr]);
    b.inst(
        OP_PHI,
        &[
            ty_u32,
            d_rem_phi,
            p_ndim,
            lbl_in_elem,
            d_rem_next,
            lbl_dec_cont,
        ],
    );
    b.inst(
        OP_PHI,
        &[ty_u32, rem_phi, tid, lbl_in_elem, rem_next, lbl_dec_cont],
    );
    let dec_cond = b.alloc_id();
    b.inst(OP_UGREATER_THAN, &[ty_bool, dec_cond, d_rem_phi, c_u0]);
    b.inst(OP_LOOP_MERGE, &[lbl_dec_exit, lbl_dec_cont, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[dec_cond, lbl_dec_body, lbl_dec_exit],
    );

    b.inst(OP_LABEL, &[lbl_dec_body]);
    b.inst(OP_ISUB, &[ty_u32, d_rem_next, d_rem_phi, c_u1]);
    let dim_sz = b.load_buf(ptr_unif_u32, ty_u32, var_oshape, c_u0, d_rem_next);
    let coord = b.alloc_id();
    b.inst(OP_UMOD, &[ty_u32, coord, rem_phi, dim_sz]);
    b.inst(OP_UDIV, &[ty_u32, rem_next, rem_phi, dim_sz]);
    let pc = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_u32, pc, var_coords, d_rem_next]);
    b.inst(OP_STORE, &[pc, coord]);
    b.inst(OP_BRANCH, &[lbl_dec_cont]);

    b.inst(OP_LABEL, &[lbl_dec_cont]);
    b.inst(OP_BRANCH, &[lbl_dec_hdr]);

    b.inst(OP_LABEL, &[lbl_dec_exit]);
    b.inst(OP_BRANCH, &[lbl_in_hdr]);

    let inp_phi = b.alloc_id();
    let inp_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_in_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, inp_phi, c_u0, lbl_dec_exit, inp_next, lbl_in_cont],
    );
    let in_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, in_cond, inp_phi, p_n_in]);
    b.inst(OP_LOOP_MERGE, &[lbl_in_exit, lbl_in_cont, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[in_cond, lbl_in_body, lbl_in_exit]);

    b.inst(OP_LABEL, &[lbl_in_body]);
    let inp_ndim = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, inp_ndim, inp_phi, p_ndim]);
    b.inst(OP_BRANCH, &[lbl_st_hdr]);

    let sd_phi = b.alloc_id();
    let off_phi = b.alloc_id();
    let sd_next = b.alloc_id();
    let off_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_st_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, sd_phi, c_u0, lbl_in_body, sd_next, lbl_st_cont],
    );
    b.inst(
        OP_PHI,
        &[ty_u32, off_phi, c_u0, lbl_in_body, off_next, lbl_st_cont],
    );
    let st_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, st_cond, sd_phi, p_ndim]);
    b.inst(OP_LOOP_MERGE, &[lbl_st_exit, lbl_st_cont, 0]);
    b.inst(OP_BRANCH_CONDITIONAL, &[st_cond, lbl_st_body, lbl_st_exit]);

    b.inst(OP_LABEL, &[lbl_st_body]);
    let pc2 = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_u32, pc2, var_coords, sd_phi]);
    let cv = b.alloc_id();
    b.inst(OP_LOAD, &[ty_u32, cv, pc2]);
    let s_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, s_idx, inp_ndim, sd_phi]);
    let stride = b.load_buf(ptr_unif_u32, ty_u32, var_strides, c_u0, s_idx);
    let term = b.alloc_id();
    b.inst(OP_IMUL, &[ty_u32, term, cv, stride]);
    b.inst(OP_IADD, &[ty_u32, off_next, off_phi, term]);
    b.inst(OP_BRANCH, &[lbl_st_cont]);

    b.inst(OP_LABEL, &[lbl_st_cont]);
    b.inst(OP_IADD, &[ty_u32, sd_next, sd_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_st_hdr]);

    b.inst(OP_LABEL, &[lbl_st_exit]);
    b.inst(OP_SELECTION_MERGE, &[lbl_in_fetch_merge, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[is_packed, lbl_in_packed, lbl_in_direct],
    );

    b.inst(OP_LABEL, &[lbl_in_packed]);
    let ob_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, ob_idx, offsets_base, inp_phi]);
    let base_off = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, ob_idx);
    let pk_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, pk_idx, base_off, off_phi]);
    let pk_val = spv_unpack_half(&mut b, &hc, is_bf16, var_in0, pk_idx);
    b.inst(OP_STORE, &[var_in_val, pk_val]);
    b.inst(OP_BRANCH, &[lbl_in_fetch_merge]);

    b.inst(OP_LABEL, &[lbl_in_direct]);
    b.inst(OP_SELECTION_MERGE, &[lbl_in_sw_merge, 0]);
    b.inst(
        OP_SWITCH,
        &[inp_phi, lbl_in_c3, 0, lbl_in_c0, 1, lbl_in_c1, 2, lbl_in_c2],
    );
    for (lbl, var_in) in [
        (lbl_in_c0, var_in0),
        (lbl_in_c1, var_in1),
        (lbl_in_c2, var_in2),
        (lbl_in_c3, var_in3),
    ] {
        b.inst(OP_LABEL, &[lbl]);
        let v = spv_unpack_half(&mut b, &hc, is_bf16, var_in, off_phi);
        b.inst(OP_STORE, &[var_in_val, v]);
        b.inst(OP_BRANCH, &[lbl_in_sw_merge]);
    }
    b.inst(OP_LABEL, &[lbl_in_sw_merge]);
    b.inst(OP_BRANCH, &[lbl_in_fetch_merge]);

    b.inst(OP_LABEL, &[lbl_in_fetch_merge]);
    let loaded_in = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, loaded_in, var_in_val]);
    let ps_in = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, ps_in, var_slots, inp_phi]);
    b.inst(OP_STORE, &[ps_in, loaded_in]);
    b.inst(OP_BRANCH, &[lbl_in_cont]);

    b.inst(OP_LABEL, &[lbl_in_cont]);
    b.inst(OP_IADD, &[ty_u32, inp_next, inp_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_in_hdr]);

    b.inst(OP_LABEL, &[lbl_in_exit]);
    b.inst(OP_BRANCH, &[lbl_step_hdr]);

    let s_phi = b.alloc_id();
    let s_next = b.alloc_id();
    b.inst(OP_LABEL, &[lbl_step_hdr]);
    b.inst(
        OP_PHI,
        &[ty_u32, s_phi, c_u0, lbl_in_exit, s_next, lbl_step_cont],
    );
    let s_cond = b.alloc_id();
    b.inst(OP_ULESS_THAN, &[ty_bool, s_cond, s_phi, p_n_steps]);
    b.inst(OP_LOOP_MERGE, &[lbl_step_exit, lbl_step_cont, 0]);
    b.inst(
        OP_BRANCH_CONDITIONAL,
        &[s_cond, lbl_step_body, lbl_step_exit],
    );

    b.inst(OP_LABEL, &[lbl_step_body]);
    let op_v = b.load_buf(ptr_unif_u32, ty_u32, var_ops, c_u0, s_phi);
    let a_slot = b.load_buf(ptr_unif_u32, ty_u32, var_arga, c_u0, s_phi);
    let b_slot = b.load_buf(ptr_unif_u32, ty_u32, var_argb, c_u0, s_phi);
    let imm_idx = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, imm_idx, c_u5, s_phi]);
    let imm_bits = b.load_buf(ptr_unif_u32, ty_u32, var_meta, c_u0, imm_idx);
    let imm = b.alloc_id();
    b.inst(OP_BITCAST, &[ty_f32, imm, imm_bits]);

    let pa = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, pa, var_slots, a_slot]);
    let va = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, va, pa]);
    let pb = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, pb, var_slots, b_slot]);
    let vb = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, vb, pb]);

    let emit_safe_pow = |b: &mut SpirvBuilder, base: u32, exp: u32| -> u32 {
        let is_sq = b.alloc_id();
        b.inst(OP_FORD_EQUAL, &[ty_bool, is_sq, exp, c_f2]);
        let is_cu = b.alloc_id();
        b.inst(OP_FORD_EQUAL, &[ty_bool, is_cu, exp, c_f3]);
        let sq = b.alloc_id();
        b.inst(OP_FMUL, &[ty_f32, sq, base, base]);
        let cu = b.alloc_id();
        b.inst(OP_FMUL, &[ty_f32, cu, sq, base]);
        let abs_b = b.alloc_id();
        b.inst(OP_EXT_INST, &[ty_f32, abs_b, glsl_ext, GLSL_FABS, base]);
        let is_z = b.alloc_id();
        b.inst(OP_FORD_EQUAL, &[ty_bool, is_z, abs_b, c_f0]);
        let safe_b = b.alloc_id();
        b.inst(OP_SELECT, &[ty_f32, safe_b, is_z, c_f1, abs_b]);
        let gen_p = b.alloc_id();
        b.inst(
            OP_EXT_INST,
            &[ty_f32, gen_p, glsl_ext, GLSL_POW, safe_b, exp],
        );
        let gen_z = b.alloc_id();
        b.inst(OP_SELECT, &[ty_f32, gen_z, is_z, c_f0, gen_p]);
        let r1 = b.alloc_id();
        b.inst(OP_SELECT, &[ty_f32, r1, is_cu, cu, gen_z]);
        let r2 = b.alloc_id();
        b.inst(OP_SELECT, &[ty_f32, r2, is_sq, sq, r1]);
        r2
    };

    let mut sw_ops = vec![op_v, op_lbls[24]];
    for code in 0..24u32 {
        sw_ops.push(code);
        sw_ops.push(op_lbls[code as usize]);
    }
    b.inst(OP_SELECTION_MERGE, &[lbl_step_merge, 0]);
    b.inst(OP_SWITCH, &sw_ops);

    let finish_case = |b: &mut SpirvBuilder, res: u32| {
        b.inst(OP_STORE, &[var_step_res, res]);
        b.inst(OP_BRANCH, &[lbl_step_merge]);
    };

    b.inst(OP_LABEL, &[op_lbls[0]]);
    let r = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, r, va, vb]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[1]]);
    let r = b.alloc_id();
    b.inst(OP_FSUB, &[ty_f32, r, va, vb]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[2]]);
    let r = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, r, va, vb]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[3]]);
    let r = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, r, va, vb]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[4]]);
    let r = emit_safe_pow(&mut b, va, vb);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[5]]);
    let r = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, r, va, imm]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[6]]);
    let r = b.alloc_id();
    b.inst(OP_FSUB, &[ty_f32, r, va, imm]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[7]]);
    let r = b.alloc_id();
    b.inst(OP_FSUB, &[ty_f32, r, imm, va]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[8]]);
    let r = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, r, va, imm]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[9]]);
    let r = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, r, va, imm]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[10]]);
    let r = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, r, imm, va]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[11]]);
    let r = emit_safe_pow(&mut b, va, imm);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[12]]);
    let r = emit_safe_pow(&mut b, imm, va);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[13]]);
    let r = b.alloc_id();
    b.inst(OP_FNEGATE, &[ty_f32, r, va]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[14]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_FMAX, va, c_f0]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[15]]);
    let x2 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, x2, va, va]);
    let x3 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, x3, x2, va]);
    let cx3 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, cx3, c_gelu_b, x3]);
    let i1 = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, i1, va, cx3]);
    let i2 = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, i2, c_gelu_a, i1]);
    let th = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, th, glsl_ext, GLSL_TANH, i2]);
    let op_th = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, op_th, c_f1, th]);
    let hx = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, hx, c_f05, va]);
    let r = b.alloc_id();
    b.inst(OP_FMUL, &[ty_f32, r, hx, op_th]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[16]]);
    let na = b.alloc_id();
    b.inst(OP_FNEGATE, &[ty_f32, na, va]);
    let nac = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, nac, glsl_ext, GLSL_FCLAMP, na, c_f_lo, c_f_hi],
    );
    let ena = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, ena, glsl_ext, GLSL_EXP, nac]);
    let ope = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, ope, c_f1, ena]);
    let r = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, r, va, ope]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[17]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_TANH, va]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[18]]);
    let na = b.alloc_id();
    b.inst(OP_FNEGATE, &[ty_f32, na, va]);
    let nac = b.alloc_id();
    b.inst(
        OP_EXT_INST,
        &[ty_f32, nac, glsl_ext, GLSL_FCLAMP, na, c_f_lo, c_f_hi],
    );
    let ena = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, ena, glsl_ext, GLSL_EXP, nac]);
    let ope = b.alloc_id();
    b.inst(OP_FADD, &[ty_f32, ope, c_f1, ena]);
    let r = b.alloc_id();
    b.inst(OP_FDIV, &[ty_f32, r, c_f1, ope]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[19]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_EXP, va]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[20]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_LOG, va]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[21]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_SQRT, va]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[22]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_SIN, va]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[23]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_COS, va]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[op_lbls[24]]);
    let r = b.alloc_id();
    b.inst(OP_EXT_INST, &[ty_f32, r, glsl_ext, GLSL_FABS, va]);
    finish_case(&mut b, r);

    b.inst(OP_LABEL, &[lbl_step_merge]);
    let sres = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, sres, var_step_res]);
    let dst_slot = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, dst_slot, p_n_in, s_phi]);
    let p_dst = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, p_dst, var_slots, dst_slot]);
    b.inst(OP_STORE, &[p_dst, sres]);
    b.inst(OP_BRANCH, &[lbl_step_cont]);

    b.inst(OP_LABEL, &[lbl_step_cont]);
    b.inst(OP_IADD, &[ty_u32, s_next, s_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_step_hdr]);

    b.inst(OP_LABEL, &[lbl_step_exit]);
    let last_s0 = b.alloc_id();
    b.inst(OP_IADD, &[ty_u32, last_s0, p_n_in, p_n_steps]);
    let last_slot = b.alloc_id();
    b.inst(OP_ISUB, &[ty_u32, last_slot, last_s0, c_u1]);
    let p_last = b.alloc_id();
    b.inst(OP_ACCESS_CHAIN, &[ptr_fn_f32, p_last, var_slots, last_slot]);
    let final_v = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, final_v, p_last]);
    let p_lane_dst = b.alloc_id();
    b.inst(
        OP_ACCESS_CHAIN,
        &[ptr_fn_f32, p_lane_dst, var_lane_out, lane_phi],
    );
    b.inst(OP_STORE, &[p_lane_dst, final_v]);
    b.inst(OP_BRANCH, &[lbl_lane_merge]);

    b.inst(OP_LABEL, &[lbl_lane_merge]);
    b.inst(OP_BRANCH, &[lbl_lane_cont]);

    b.inst(OP_LABEL, &[lbl_lane_cont]);
    b.inst(OP_IADD, &[ty_u32, lane_next, lane_phi, c_u1]);
    b.inst(OP_BRANCH, &[lbl_lane_hdr]);

    b.inst(OP_LABEL, &[lbl_lane_exit]);
    let v0 = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, v0, p_lo0]);
    let v1 = b.alloc_id();
    b.inst(OP_LOAD, &[ty_f32, v1, p_lo1]);
    let packed = spv_pack_half_pair(&mut b, &hc, is_bf16, v0, v1);
    b.store_buf(ptr_unif_u32, var_out, c_u0, pair_idx, packed);
    b.inst(OP_BRANCH, &[lbl_exit]);

    b.inst(OP_LABEL, &[lbl_exit]);
    b.inst(OP_RETURN, &[]);
    b.inst(OP_FUNCTION_END, &[]);
    b.finish()
}
