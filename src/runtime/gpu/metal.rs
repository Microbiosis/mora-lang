//! v0.110.1: Native Apple Metal Framework (`Metal.framework`) Hardware Backend (macOS).
//!
//! Architecture:
//! - Zero compile-time SDK / Cargo dependencies: on `#[cfg(target_os = "macos")]`, dynamically loads
//!   `/System/Library/Frameworks/Metal.framework/Metal` and `/usr/lib/libobjc.A.dylib`, obtains the
//!   default `MTLDevice` via `MTLCreateSystemDefaultDevice()`, compiles Mora's 6 MSL (Metal Shading
//!   Language) compute kernels at runtime via `newLibraryWithSource:options:error:`, and dispatches
//!   persistent VRAM-resident `MatMul` (2D & batched ND in `f32`/`f16`/`bf16`), `FusedLinearAct`
//!   (`f32`/`f16`/`bf16`), `FusedRmsNormLinear`, `FusedFlashAttention`, and `FusedElementwise`
//!   directly on Apple Silicon / AMD Metal GPUs with zero host `simd_kernels` fallback.
//! - On non-macOS hosts (Windows/Linux), cleanly returns empty hardware list so `metal:N` tagged
//!   graphs transparently fall back to `EmulatedDeviceBackend`.

use crate::runtime::gpu::HardwareBackendEntry;

#[cfg(any(target_os = "macos", test))]
const MORA_METAL_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

static inline float mora_apply_act(float acc, uint act) {
    if (act == 1u) {
        return max(acc, 0.0f);
    } else if (act == 2u) {
        float c = 0.79788456f * (acc + 0.044715f * acc * acc * acc);
        return 0.5f * acc * (1.0f + tanh(c));
    } else if (act == 3u) {
        return acc / (1.0f + exp(clamp(-acc, -40.0f, 40.0f)));
    } else if (act == 4u) {
        return tanh(acc);
    } else if (act == 5u) {
        return 1.0f / (1.0f + exp(clamp(-acc, -40.0f, 40.0f)));
    }
    return acc;
}

static inline float mora_bf16_to_f32(ushort bits) {
    return as_type<float>(uint(bits) << 16u);
}

static inline ushort mora_f32_to_bf16(float x) {
    uint u = as_type<uint>(x);
    if ((u & 0x7f800000u) == 0x7f800000u && (u & 0x007fffffu) != 0u) {
        return ushort((u >> 16u) | 0x0040u);
    }
    uint lsb = (u >> 16u) & 1u;
    uint rounded = u + 0x7fffu + lsb;
    return ushort(rounded >> 16u);
}

static inline float mora_safe_pow(float x, float y) {
    if (y == 2.0f) return x * x;
    if (y == 3.0f) return x * x * x;
    if (y == 0.0f) return 1.0f;
    if (y == 1.0f) return x;
    if (x < 0.0f) {
        float ry = rint(y);
        float p = pow(-x, y);
        return (fabs(ry - y) < 1.0e-6f && (int(ry) & 1) != 0) ? -p : p;
    }
    return pow(x, y);
}

kernel void mora_fused_linear_f32(
    device const float* A [[buffer(0)]],
    device const float* B [[buffer(1)]],
    device const float* Bias [[buffer(2)]],
    device float* Out [[buffer(3)]],
    device const uint* Params [[buffer(4)]],
    device const uint* ABases [[buffer(5)]],
    device const uint* BBases [[buffer(6)]],
    uint3 gid [[thread_position_in_grid]])
{
    uint M = Params[0];
    uint N = Params[1];
    uint K = Params[2];
    uint has_bias = Params[3];
    uint bias_len = Params[4];
    uint act = Params[5];
    uint batches = Params[6];
    uint col = gid.x;
    uint row = gid.y;
    uint batch = gid.z;
    if (row >= M || col >= N || batch >= batches) return;
    uint a_base = ABases[batch] + row * K;
    uint b_base = BBases[batch];
    float acc = 0.0f;
    for (uint t = 0u; t < K; ++t) {
        acc = fma(A[a_base + t], B[b_base + t * N + col], acc);
    }
    if (has_bias != 0u) {
        acc += Bias[col % bias_len];
    }
    acc = mora_apply_act(acc, act);
    Out[(batch * M + row) * N + col] = acc;
}

kernel void mora_fused_linear_f16(
    device const half* A [[buffer(0)]],
    device const half* B [[buffer(1)]],
    device const half* Bias [[buffer(2)]],
    device half* Out [[buffer(3)]],
    device const uint* Params [[buffer(4)]],
    device const uint* ABases [[buffer(5)]],
    device const uint* BBases [[buffer(6)]],
    uint3 gid [[thread_position_in_grid]])
{
    uint M = Params[0];
    uint N = Params[1];
    uint K = Params[2];
    uint has_bias = Params[3];
    uint bias_len = Params[4];
    uint act = Params[5];
    uint batches = Params[6];
    uint col = gid.x;
    uint row = gid.y;
    uint batch = gid.z;
    if (row >= M || col >= N || batch >= batches) return;
    uint a_base = ABases[batch] + row * K;
    uint b_base = BBases[batch];
    float acc = 0.0f;
    for (uint t = 0u; t < K; ++t) {
        acc = fma(float(A[a_base + t]), float(B[b_base + t * N + col]), acc);
    }
    if (has_bias != 0u) {
        acc += float(Bias[col % bias_len]);
    }
    acc = mora_apply_act(acc, act);
    Out[(batch * M + row) * N + col] = half(acc);
}

kernel void mora_fused_linear_bf16(
    device const ushort* A [[buffer(0)]],
    device const ushort* B [[buffer(1)]],
    device const ushort* Bias [[buffer(2)]],
    device ushort* Out [[buffer(3)]],
    device const uint* Params [[buffer(4)]],
    device const uint* ABases [[buffer(5)]],
    device const uint* BBases [[buffer(6)]],
    uint3 gid [[thread_position_in_grid]])
{
    uint M = Params[0];
    uint N = Params[1];
    uint K = Params[2];
    uint has_bias = Params[3];
    uint bias_len = Params[4];
    uint act = Params[5];
    uint batches = Params[6];
    uint col = gid.x;
    uint row = gid.y;
    uint batch = gid.z;
    if (row >= M || col >= N || batch >= batches) return;
    uint a_base = ABases[batch] + row * K;
    uint b_base = BBases[batch];
    float acc = 0.0f;
    for (uint t = 0u; t < K; ++t) {
        acc = fma(mora_bf16_to_f32(A[a_base + t]), mora_bf16_to_f32(B[b_base + t * N + col]), acc);
    }
    if (has_bias != 0u) {
        acc += mora_bf16_to_f32(Bias[col % bias_len]);
    }
    acc = mora_apply_act(acc, act);
    Out[(batch * M + row) * N + col] = mora_f32_to_bf16(acc);
}

kernel void mora_rms_norm_f32(
    device const float* X [[buffer(0)]],
    device const float* Gamma [[buffer(1)]],
    device float* Out [[buffer(2)]],
    device const uint* Params [[buffer(3)]],
    uint gid [[thread_position_in_grid]])
{
    uint M = Params[0];
    uint K = Params[1];
    uint has_gamma = Params[2];
    uint gamma_len = Params[3];
    float eps = as_type<float>(Params[4]);
    if (gid >= M) return;
    uint base = gid * K;
    float sq = 0.0f;
    for (uint j = 0u; j < K; ++j) {
        float v = X[base + j];
        sq = fma(v, v, sq);
    }
    float inv = rsqrt(sq / float(K) + eps);
    for (uint j = 0u; j < K; ++j) {
        float g = (has_gamma != 0u) ? Gamma[j % gamma_len] : 1.0f;
        Out[base + j] = X[base + j] * inv * g;
    }
}

kernel void mora_fused_flash_attention_f32(
    device const float* Q [[buffer(0)]],
    device const float* K [[buffer(1)]],
    device const float* V [[buffer(2)]],
    device const float* Mask [[buffer(3)]],
    device float* Out [[buffer(4)]],
    device const uint* QBases [[buffer(5)]],
    device const uint* KBases [[buffer(6)]],
    device const uint* VBases [[buffer(7)]],
    device const uint* MBases [[buffer(8)]],
    device const uint* Params [[buffer(9)]],
    uint3 gid [[thread_position_in_grid]])
{
    uint batches = Params[0];
    uint s_q = Params[1];
    uint s_k = Params[2];
    uint d_k = Params[3];
    uint d_v = Params[4];
    uint k_transposed = Params[5];
    uint has_mask = Params[6];
    uint mask_sq_s = Params[7];
    uint mask_sk_s = Params[8];
    float scale = as_type<float>(Params[9]);
    uint dv = gid.x;
    uint q_pos = gid.y;
    uint batch = gid.z;
    if (dv >= d_v || q_pos >= s_q || batch >= batches) return;

    uint q_row = QBases[batch] + q_pos * d_k;
    uint k_base = KBases[batch];
    uint v_base = VBases[batch];
    uint m_base = MBases[batch];

    float m_max = -1.0e38f;
    float l_sum = 0.0f;
    float acc = 0.0f;
    for (uint j = 0u; j < s_k; ++j) {
        float dot = 0.0f;
        if (k_transposed != 0u) {
            for (uint d = 0u; d < d_k; ++d) {
                dot = fma(Q[q_row + d], K[k_base + d * s_k + j], dot);
            }
        } else {
            uint k_row = k_base + j * d_k;
            for (uint d = 0u; d < d_k; ++d) {
                dot = fma(Q[q_row + d], K[k_row + d], dot);
            }
        }
        float score = dot * scale;
        if (has_mask != 0u) {
            score += Mask[m_base + q_pos * mask_sq_s + j * mask_sk_s];
        }
        float new_max = max(m_max, score);
        float alpha = exp(m_max - new_max);
        float p = exp(score - new_max);
        l_sum = l_sum * alpha + p;
        acc = acc * alpha + p * V[v_base + j * d_v + dv];
        m_max = new_max;
    }
    Out[(batch * s_q + q_pos) * d_v + dv] = (l_sum > 0.0f) ? (acc / l_sum) : 0.0f;
}

kernel void mora_fused_elementwise_f32(
    device float* Out [[buffer(0)]],
    device const float* In0 [[buffer(1)]],
    device const float* In1 [[buffer(2)]],
    device const float* In2 [[buffer(3)]],
    device const float* In3 [[buffer(4)]],
    device const uint* Strides [[buffer(5)]],
    device const uint* OutShape [[buffer(6)]],
    device const uint* Ops [[buffer(7)]],
    device const uint* ArgA [[buffer(8)]],
    device const uint* ArgB [[buffer(9)]],
    device const uint* Meta [[buffer(10)]],
    uint gid [[thread_position_in_grid]])
{
    uint ndim = Meta[0];
    uint num_inputs = Meta[1];
    uint num_steps = Meta[2];
    uint out_numel = Meta[3];
    uint packed_mode = Meta[4];
    if (gid >= out_numel) return;

    uint coords[16];
    uint rem = gid;
    for (uint d = ndim; d > 0u; --d) {
        uint idx = d - 1u;
        uint dim = OutShape[idx];
        coords[idx] = rem % dim;
        rem /= dim;
    }

    float slots[64];
    uint off_base = 5u + num_steps;
    for (uint i = 0u; i < num_inputs; ++i) {
        uint flat = 0u;
        uint s_off = i * ndim;
        for (uint d = 0u; d < ndim; ++d) {
            flat += coords[d] * Strides[s_off + d];
        }
        if (packed_mode != 0u) {
            slots[i] = In0[Meta[off_base + i] + flat];
        } else if (i == 0u) {
            slots[i] = In0[flat];
        } else if (i == 1u) {
            slots[i] = In1[flat];
        } else if (i == 2u) {
            slots[i] = In2[flat];
        } else {
            slots[i] = In3[flat];
        }
    }

    for (uint s = 0u; s < num_steps; ++s) {
        uint op = Ops[s];
        float a = slots[ArgA[s]];
        float b = slots[ArgB[s]];
        float imm = as_type<float>(Meta[5u + s]);
        float r = 0.0f;
        switch (op) {
            case 0u: r = a + b; break;
            case 1u: r = a - b; break;
            case 2u: r = a * b; break;
            case 3u: r = a / b; break;
            case 4u: r = mora_safe_pow(a, b); break;
            case 5u: r = a + imm; break;
            case 6u: r = a - imm; break;
            case 7u: r = imm - a; break;
            case 8u: r = a * imm; break;
            case 9u: r = a / imm; break;
            case 10u: r = imm / a; break;
            case 11u: r = mora_safe_pow(a, imm); break;
            case 12u: r = mora_safe_pow(imm, a); break;
            case 13u: r = -a; break;
            case 14u: r = max(a, 0.0f); break;
            case 15u: {
                float c = 0.79788456f * (a + 0.044715f * a * a * a);
                r = 0.5f * a * (1.0f + tanh(c));
                break;
            }
            case 16u: r = a / (1.0f + exp(clamp(-a, -40.0f, 40.0f))); break;
            case 17u: r = tanh(a); break;
            case 18u: r = 1.0f / (1.0f + exp(clamp(-a, -40.0f, 40.0f))); break;
            case 19u: r = exp(a); break;
            case 20u: r = log(a); break;
            case 21u: r = sqrt(a); break;
            case 22u: r = sin(a); break;
            case 23u: r = cos(a); break;
            case 24u: r = fabs(a); break;
            default: r = a; break;
        }
        slots[num_inputs + s] = r;
    }
    Out[gid] = slots[num_inputs + num_steps - 1u];
}
"#;

#[cfg(any(target_os = "macos", test))]
fn pack_metal_custom_kernel_scalars(
    args: &[crate::runtime::gpu::kernel_compiler::KernelLaunchArg],
) -> Vec<u32> {
    use crate::runtime::gpu::kernel_compiler::KernelLaunchArg;
    let mut words = Vec::new();
    for arg in args {
        match arg {
            KernelLaunchArg::VramBuffer { .. } => {}
            KernelLaunchArg::F32(v) => words.push(v.to_bits()),
            KernelLaunchArg::F64(v) => words.push((*v as f32).to_bits()),
            KernelLaunchArg::I32(v) => words.push(*v as u32),
        }
    }
    words
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn probe_metal_backends() -> Vec<HardwareBackendEntry> {
    Vec::new()
}

#[cfg(target_os = "macos")]
mod macos_impl {
    use std::collections::HashMap;
    use std::ffi::{CStr, c_char, c_void};
    use std::ptr;
    use std::sync::Arc;

    use crate::common::kernel_ir::KernelProgram;
    use crate::common::sync::Mutex;
    use crate::common::{
        DType, Device, bf16_bits_to_f64, f16_bits_to_f64, f64_to_bf16_bits, f64_to_f16_bits,
    };
    use crate::error::{MoraError, MoraResult};
    use crate::runtime::compute_dag::{
        DeviceBackend, DeviceVramBuffer, FusedAct, FusedEwStep, VramTelemetry,
    };
    use crate::runtime::gpu::kernel_compiler::{KernelLaunchArg, emit_msl};
    use crate::runtime::gpu::{DynLib, GpuDeviceInfo, HardwareBackendEntry, VramCounters};
    use crate::runtime::simd_kernels;

    use super::{MORA_METAL_MSL, pack_metal_custom_kernel_scalars};

    type Id = *mut c_void;
    type Sel = *mut c_void;
    type FnMtlCreateDefaultDevice = unsafe extern "C" fn() -> Id;
    type FnObjcGetClass = unsafe extern "C" fn(*const c_char) -> Id;
    type FnSelRegisterName = unsafe extern "C" fn(*const c_char) -> Sel;
    type FnMsgSend0 = unsafe extern "C" fn(Id, Sel) -> Id;
    type FnMsgSendU64 = unsafe extern "C" fn(Id, Sel) -> u64;
    type FnMsgSendCStr = unsafe extern "C" fn(Id, Sel) -> *const c_char;
    type FnMsgSendVoid = unsafe extern "C" fn(Id, Sel);
    type FnMsgSendInitBytes = unsafe extern "C" fn(Id, Sel, *const c_void, usize, usize) -> Id;
    type FnMsgSendNewLib = unsafe extern "C" fn(Id, Sel, Id, Id, *mut Id) -> Id;
    type FnMsgSendNewFn = unsafe extern "C" fn(Id, Sel, Id) -> Id;
    type FnMsgSendNewPipe = unsafe extern "C" fn(Id, Sel, Id, *mut Id) -> Id;
    type FnMsgSendNewBuf = unsafe extern "C" fn(Id, Sel, *const c_void, usize, u64) -> Id;
    type FnMsgSendNewBufLen = unsafe extern "C" fn(Id, Sel, usize, u64) -> Id;
    type FnMsgSendSetPipe = unsafe extern "C" fn(Id, Sel, Id);
    type FnMsgSendSetBuf = unsafe extern "C" fn(Id, Sel, Id, usize, usize);

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct MtlSize {
        width: usize,
        height: usize,
        depth: usize,
    }
    type FnMsgSendDispatch = unsafe extern "C" fn(Id, Sel, MtlSize, MtlSize);

    struct ObjC {
        _metal_lib: DynLib,
        _objc_lib: DynLib,
        get_class: FnObjcGetClass,
        sel: FnSelRegisterName,
        msg0: FnMsgSend0,
        msg_u64: FnMsgSendU64,
        msg_cstr: FnMsgSendCStr,
        msg_void: FnMsgSendVoid,
        msg_init_bytes: FnMsgSendInitBytes,
        msg_new_lib: FnMsgSendNewLib,
        msg_new_fn: FnMsgSendNewFn,
        msg_new_pipe: FnMsgSendNewPipe,
        msg_new_buf: FnMsgSendNewBuf,
        msg_new_buf_len: FnMsgSendNewBufLen,
        msg_set_pipe: FnMsgSendSetPipe,
        msg_set_buf: FnMsgSendSetBuf,
        msg_dispatch: FnMsgSendDispatch,
        create_default_device: FnMtlCreateDefaultDevice,
    }

    impl ObjC {
        fn load() -> Option<Arc<Self>> {
            let metal_lib = DynLib::open("/System/Library/Frameworks/Metal.framework/Metal")?;
            let objc_lib = DynLib::open("/usr/lib/libobjc.A.dylib")?;
            // SAFETY: Standard macOS Metal & Objective-C runtime entry points.
            unsafe {
                let create_default_device: FnMtlCreateDefaultDevice =
                    metal_lib.get_symbol(b"MTLCreateSystemDefaultDevice\0")?;
                let get_class: FnObjcGetClass = objc_lib.get_symbol(b"objc_getClass\0")?;
                let sel: FnSelRegisterName = objc_lib.get_symbol(b"sel_registerName\0")?;
                let msg0: FnMsgSend0 = objc_lib.get_symbol(b"objc_msgSend\0")?;
                let msg_u64: FnMsgSendU64 = objc_lib.get_symbol(b"objc_msgSend\0")?;
                let msg_cstr: FnMsgSendCStr = objc_lib.get_symbol(b"objc_msgSend\0")?;
                let msg_void: FnMsgSendVoid = objc_lib.get_symbol(b"objc_msgSend\0")?;
                let msg_init_bytes: FnMsgSendInitBytes = objc_lib.get_symbol(b"objc_msgSend\0")?;
                let msg_new_lib: FnMsgSendNewLib = objc_lib.get_symbol(b"objc_msgSend\0")?;
                let msg_new_fn: FnMsgSendNewFn = objc_lib.get_symbol(b"objc_msgSend\0")?;
                let msg_new_pipe: FnMsgSendNewPipe = objc_lib.get_symbol(b"objc_msgSend\0")?;
                let msg_new_buf: FnMsgSendNewBuf = objc_lib.get_symbol(b"objc_msgSend\0")?;
                let msg_new_buf_len: FnMsgSendNewBufLen = objc_lib.get_symbol(b"objc_msgSend\0")?;
                let msg_set_pipe: FnMsgSendSetPipe = objc_lib.get_symbol(b"objc_msgSend\0")?;
                let msg_set_buf: FnMsgSendSetBuf = objc_lib.get_symbol(b"objc_msgSend\0")?;
                let msg_dispatch: FnMsgSendDispatch = objc_lib.get_symbol(b"objc_msgSend\0")?;
                Some(Arc::new(Self {
                    _metal_lib: metal_lib,
                    _objc_lib: objc_lib,
                    get_class,
                    sel,
                    msg0,
                    msg_u64,
                    msg_cstr,
                    msg_void,
                    msg_init_bytes,
                    msg_new_lib,
                    msg_new_fn,
                    msg_new_pipe,
                    msg_new_buf,
                    msg_new_buf_len,
                    msg_set_pipe,
                    msg_set_buf,
                    msg_dispatch,
                    create_default_device,
                }))
            }
        }

        fn make_nsstring(&self, s: &str) -> Id {
            // SAFETY: Allocates and initializes an owned `NSString` from UTF-8 bytes (encoding = 4).
            unsafe {
                let cls = (self.get_class)(b"NSString\0".as_ptr() as *const c_char);
                let s_alloc = (self.sel)(b"alloc\0".as_ptr() as *const c_char);
                let s_init =
                    (self.sel)(b"initWithBytes:length:encoding:\0".as_ptr() as *const c_char);
                let raw = (self.msg0)(cls, s_alloc);
                (self.msg_init_bytes)(raw, s_init, s.as_ptr() as *const c_void, s.len(), 4)
            }
        }

        fn release(&self, obj: Id) {
            if !obj.is_null() {
                // SAFETY: Releases a retained Objective-C object created via alloc/new.
                unsafe {
                    let s_rel = (self.sel)(b"release\0".as_ptr() as *const c_char);
                    (self.msg_void)(obj, s_rel);
                }
            }
        }
    }

    struct MetalState {
        mtl_dev: Id,
        queue: Id,
        pipe_linear_f32: Id,
        pipe_linear_f16: Id,
        pipe_linear_bf16: Id,
        pipe_rms_norm_f32: Id,
        pipe_flash_attn_f32: Id,
        pipe_fused_ew_f32: Id,
        dummy_buf: Id,
        custom_pipes: HashMap<u64, Id>,
    }

    struct MetalDeviceInner {
        device: Device,
        objc: Arc<ObjC>,
        state: Mutex<MetalState>,
        counters: VramCounters,
    }

    // SAFETY: All Metal device/queue/pipeline ObjC messages are serialized under `self.state.lock()`.
    unsafe impl Send for MetalDeviceInner {}
    // SAFETY: Serialized access via `Mutex<MetalState>` and atomic `VramCounters`.
    unsafe impl Sync for MetalDeviceInner {}

    impl Drop for MetalDeviceInner {
        fn drop(&mut self) {
            let mut st = self.state.lock();
            for (_, pipe) in st.custom_pipes.drain() {
                self.objc.release(pipe);
            }
            self.objc.release(st.dummy_buf);
            self.objc.release(st.pipe_fused_ew_f32);
            self.objc.release(st.pipe_flash_attn_f32);
            self.objc.release(st.pipe_rms_norm_f32);
            self.objc.release(st.pipe_linear_bf16);
            self.objc.release(st.pipe_linear_f16);
            self.objc.release(st.pipe_linear_f32);
            self.objc.release(st.queue);
            self.objc.release(st.mtl_dev);
        }
    }

    /// Persistent RAII Metal `MTLBuffer` owned by a VRAM-resident [`Tensor`](crate::value::Tensor)
    /// or [`KVPageAllocator`](crate::runtime::kv_cache::KVPageAllocator) page.
    struct MetalVramBuffer {
        inner: Arc<MetalDeviceInner>,
        buf: Id,
        byte_len: usize,
    }

    // SAFETY: `MTLBuffer` handle is valid for the lifetime of `self.inner` and synchronized under `inner.state`.
    unsafe impl Send for MetalVramBuffer {}
    // SAFETY: All host copies and releases serialize on `self.inner.state.lock()`.
    unsafe impl Sync for MetalVramBuffer {}

    impl std::fmt::Debug for MetalVramBuffer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("MetalVramBuffer")
                .field("device", &self.inner.device)
                .field("buf", &(self.buf as u64))
                .field("byte_len", &self.byte_len)
                .finish()
        }
    }

    impl Drop for MetalVramBuffer {
        fn drop(&mut self) {
            let _st = self.inner.state.lock();
            self.inner.objc.release(self.buf);
            self.inner.counters.on_free(self.byte_len);
        }
    }

    impl DeviceVramBuffer for MetalVramBuffer {
        fn device(&self) -> &Device {
            &self.inner.device
        }

        fn byte_len(&self) -> usize {
            self.byte_len
        }

        fn raw_handle(&self) -> u64 {
            self.buf as u64
        }

        fn upload_bytes(&self, src: &[u8]) -> MoraResult<()> {
            if src.len() != self.byte_len {
                return Err(MoraError::Runtime(format!(
                    "Metal VRAM upload byte-length mismatch: expected {}, got {}",
                    self.byte_len,
                    src.len()
                )));
            }
            if src.is_empty() {
                return Ok(());
            }
            let _st = self.inner.state.lock();
            // SAFETY: `self.buf` is a shared `MTLBuffer` with at least `self.byte_len` bytes.
            unsafe {
                let s_contents = (self.inner.objc.sel)(b"contents\0".as_ptr() as *const c_char);
                let dst = (self.inner.objc.msg0)(self.buf, s_contents) as *mut u8;
                if dst.is_null() {
                    return Err(MoraError::Runtime(
                        "Metal VRAM buffer contents() returned null".to_string(),
                    ));
                }
                ptr::copy_nonoverlapping(src.as_ptr(), dst, src.len());
            }
            self.inner.counters.on_h2d();
            Ok(())
        }

        fn download_bytes(&self, dst: &mut [u8]) -> MoraResult<()> {
            if dst.len() != self.byte_len {
                return Err(MoraError::Runtime(format!(
                    "Metal VRAM download byte-length mismatch: expected {}, got {}",
                    self.byte_len,
                    dst.len()
                )));
            }
            if dst.is_empty() {
                return Ok(());
            }
            let _st = self.inner.state.lock();
            // SAFETY: `self.buf` is a shared `MTLBuffer` with at least `self.byte_len` bytes and prior dispatches waited.
            unsafe {
                let s_contents = (self.inner.objc.sel)(b"contents\0".as_ptr() as *const c_char);
                let src = (self.inner.objc.msg0)(self.buf, s_contents) as *const u8;
                if src.is_null() {
                    return Err(MoraError::Runtime(
                        "Metal VRAM buffer contents() returned null".to_string(),
                    ));
                }
                ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), dst.len());
            }
            self.inner.counters.on_d2h();
            Ok(())
        }
    }

    pub(crate) struct MetalDeviceBackend {
        inner: Arc<MetalDeviceInner>,
        info: GpuDeviceInfo,
    }

    fn act_code(act: FusedAct) -> u32 {
        match act {
            FusedAct::None => 0,
            FusedAct::Relu => 1,
            FusedAct::Gelu => 2,
            FusedAct::Silu => 3,
            FusedAct::Tanh => 4,
            FusedAct::Sigmoid => 5,
        }
    }

    fn encode_ew_steps(steps: &[FusedEwStep]) -> (Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>) {
        let mut ops = Vec::with_capacity(steps.len());
        let mut arg_a = Vec::with_capacity(steps.len());
        let mut arg_b = Vec::with_capacity(steps.len());
        let mut imm_bits = Vec::with_capacity(steps.len());
        for step in steps {
            let (op, a, b, im) = match *step {
                FusedEwStep::Add(a, b) => (0u32, a as u32, b as u32, 0.0f32),
                FusedEwStep::Sub(a, b) => (1, a as u32, b as u32, 0.0),
                FusedEwStep::Mul(a, b) => (2, a as u32, b as u32, 0.0),
                FusedEwStep::Div(a, b) => (3, a as u32, b as u32, 0.0),
                FusedEwStep::Pow(a, b) => (4, a as u32, b as u32, 0.0),
                FusedEwStep::AddScalar(a, s) => (5, a as u32, 0, s as f32),
                FusedEwStep::SubScalar(a, s, rev) => {
                    (if rev { 7 } else { 6 }, a as u32, 0, s as f32)
                }
                FusedEwStep::MulScalar(a, s) => (8, a as u32, 0, s as f32),
                FusedEwStep::DivScalar(a, s, rev) => {
                    (if rev { 10 } else { 9 }, a as u32, 0, s as f32)
                }
                FusedEwStep::PowScalar(a, s, rev) => {
                    (if rev { 12 } else { 11 }, a as u32, 0, s as f32)
                }
                FusedEwStep::Neg(a) => (13, a as u32, 0, 0.0),
                FusedEwStep::Relu(a) => (14, a as u32, 0, 0.0),
                FusedEwStep::Gelu(a) => (15, a as u32, 0, 0.0),
                FusedEwStep::Silu(a) => (16, a as u32, 0, 0.0),
                FusedEwStep::Tanh(a) => (17, a as u32, 0, 0.0),
                FusedEwStep::Sigmoid(a) => (18, a as u32, 0, 0.0),
                FusedEwStep::Exp(a) => (19, a as u32, 0, 0.0),
                FusedEwStep::Ln(a) => (20, a as u32, 0, 0.0),
                FusedEwStep::Sqrt(a) => (21, a as u32, 0, 0.0),
                FusedEwStep::Sin(a) => (22, a as u32, 0, 0.0),
                FusedEwStep::Cos(a) => (23, a as u32, 0, 0.0),
                FusedEwStep::Abs(a) => (24, a as u32, 0, 0.0),
            };
            ops.push(op);
            arg_a.push(a);
            arg_b.push(b);
            imm_bits.push(im.to_bits());
        }
        (ops, arg_a, arg_b, imm_bits)
    }

    fn broadcast_batch_shapes(a: &[usize], b: &[usize]) -> MoraResult<Vec<usize>> {
        let rank = a.len().max(b.len());
        let mut out = vec![1usize; rank];
        for i in 0..rank {
            let da = if i < rank - a.len() {
                1
            } else {
                a[i - (rank - a.len())]
            };
            let db = if i < rank - b.len() {
                1
            } else {
                b[i - (rank - b.len())]
            };
            if da == db {
                out[i] = da;
            } else if da == 1 {
                out[i] = db;
            } else if db == 1 {
                out[i] = da;
            } else {
                return Err(MoraError::Runtime(format!(
                    "Metal broadcast batch shape mismatch: {a:?} vs {b:?}"
                )));
            }
        }
        Ok(out)
    }

    fn map_broadcast_batch(flat_idx: usize, out_batch: &[usize], in_batch: &[usize]) -> usize {
        if in_batch.is_empty() {
            return 0;
        }
        let out_rank = out_batch.len();
        let in_rank = in_batch.len();
        let mut rem = flat_idx;
        let mut in_flat = 0usize;
        let mut in_stride = 1usize;
        for d in (0..out_rank).rev() {
            let dim = out_batch[d];
            let coord = rem % dim;
            rem /= dim;
            let offset_from_right = out_rank - 1 - d;
            if offset_from_right < in_rank {
                let in_dim = in_batch[in_rank - 1 - offset_from_right];
                let in_coord = if in_dim == 1 { 0 } else { coord };
                in_flat += in_coord * in_stride;
                in_stride *= in_dim;
            }
        }
        in_flat
    }

    fn compute_broadcast_strides_u32(in_shape: &[usize], out_shape: &[usize]) -> Vec<u32> {
        let ndim = out_shape.len();
        let mut raw_strides = vec![1u32; in_shape.len()];
        for i in (0..in_shape.len().saturating_sub(1)).rev() {
            raw_strides[i] = raw_strides[i + 1] * (in_shape[i + 1] as u32);
        }
        let mut out = vec![0u32; ndim];
        let pad = ndim.saturating_sub(in_shape.len());
        for (i, &dim) in in_shape.iter().enumerate() {
            out[pad + i] = if dim == 1 { 0 } else { raw_strides[i] };
        }
        out
    }

    impl MetalDeviceBackend {
        fn alloc_mtl_buf_len(objc: &ObjC, mtl_dev: Id, byte_len: usize) -> MoraResult<Id> {
            let alloc_len = byte_len.max(16);
            // SAFETY: Allocates a shared `MTLBuffer` on `mtl_dev`.
            let buf = unsafe {
                let s_new_len =
                    (objc.sel)(b"newBufferWithLength:options:\0".as_ptr() as *const c_char);
                (objc.msg_new_buf_len)(mtl_dev, s_new_len, alloc_len, 0)
            };
            if buf.is_null() {
                return Err(MoraError::Runtime(format!(
                    "Metal newBufferWithLength({alloc_len}) returned null"
                )));
            }
            Ok(buf)
        }

        fn upload_temp_slice<T: Copy>(
            objc: &ObjC,
            mtl_dev: Id,
            temps: &mut Vec<Id>,
            data: &[T],
        ) -> MoraResult<Id> {
            let bytes = std::mem::size_of_val(data);
            if bytes == 0 {
                let buf = Self::alloc_mtl_buf_len(objc, mtl_dev, 16)?;
                temps.push(buf);
                return Ok(buf);
            }
            // SAFETY: Copies `bytes` from `data` into a freshly allocated shared `MTLBuffer`.
            let buf = unsafe {
                let s_new_buf =
                    (objc.sel)(b"newBufferWithBytes:length:options:\0".as_ptr() as *const c_char);
                (objc.msg_new_buf)(mtl_dev, s_new_buf, data.as_ptr() as *const c_void, bytes, 0)
            };
            if buf.is_null() {
                return Err(MoraError::Runtime(format!(
                    "Metal newBufferWithBytes({bytes}) returned null"
                )));
            }
            temps.push(buf);
            Ok(buf)
        }

        fn read_mtl_buf<T: Copy>(objc: &ObjC, buf: Id, dst: &mut [T]) -> MoraResult<()> {
            if dst.is_empty() {
                return Ok(());
            }
            // SAFETY: Shared `MTLBuffer` contents are coherent after `waitUntilCompleted`.
            unsafe {
                let s_contents = (objc.sel)(b"contents\0".as_ptr() as *const c_char);
                let src = (objc.msg0)(buf, s_contents) as *const u8;
                if src.is_null() {
                    return Err(MoraError::Runtime(
                        "Metal buffer contents() returned null on read".to_string(),
                    ));
                }
                ptr::copy_nonoverlapping(
                    src,
                    dst.as_mut_ptr() as *mut u8,
                    std::mem::size_of_val(dst),
                );
            }
            Ok(())
        }

        fn alloc_owned_vram(
            &self,
            st: &MetalState,
            byte_len: usize,
        ) -> MoraResult<Arc<MetalVramBuffer>> {
            let buf = Self::alloc_mtl_buf_len(&self.inner.objc, st.mtl_dev, byte_len)?;
            self.inner.counters.on_alloc(byte_len);
            Ok(Arc::new(MetalVramBuffer {
                inner: Arc::clone(&self.inner),
                buf,
                byte_len,
            }))
        }

        fn dispatch_compute(
            objc: &ObjC,
            queue: Id,
            pipeline: Id,
            buffers: &[Id],
            grid: MtlSize,
            tg: MtlSize,
        ) -> MoraResult<()> {
            // SAFETY: Encodes a single compute pass on `queue`, binds `buffers`, commits, and waits.
            unsafe {
                let s_cmd_buf = (objc.sel)(b"commandBuffer\0".as_ptr() as *const c_char);
                let s_enc = (objc.sel)(b"computeCommandEncoder\0".as_ptr() as *const c_char);
                let s_set_pipe =
                    (objc.sel)(b"setComputePipelineState:\0".as_ptr() as *const c_char);
                let s_set_buf =
                    (objc.sel)(b"setBuffer:offset:atIndex:\0".as_ptr() as *const c_char);
                let s_dispatch = (objc.sel)(
                    b"dispatchThreads:threadsPerThreadgroup:\0".as_ptr() as *const c_char
                );
                let s_end = (objc.sel)(b"endEncoding\0".as_ptr() as *const c_char);
                let s_commit = (objc.sel)(b"commit\0".as_ptr() as *const c_char);
                let s_wait = (objc.sel)(b"waitUntilCompleted\0".as_ptr() as *const c_char);

                let cmd = (objc.msg0)(queue, s_cmd_buf);
                if cmd.is_null() {
                    return Err(MoraError::Runtime(
                        "Metal commandBuffer creation failed".to_string(),
                    ));
                }
                let enc = (objc.msg0)(cmd, s_enc);
                if enc.is_null() {
                    return Err(MoraError::Runtime(
                        "Metal computeCommandEncoder creation failed".to_string(),
                    ));
                }
                (objc.msg_set_pipe)(enc, s_set_pipe, pipeline);
                for (idx, &buf) in buffers.iter().enumerate() {
                    (objc.msg_set_buf)(enc, s_set_buf, buf, 0, idx);
                }
                (objc.msg_dispatch)(enc, s_dispatch, grid, tg);
                (objc.msg_void)(enc, s_end);
                (objc.msg_void)(cmd, s_commit);
                (objc.msg_void)(cmd, s_wait);
            }
            Ok(())
        }

        fn release_temps(objc: &ObjC, temps: Vec<Id>) {
            for b in temps {
                objc.release(b);
            }
        }

        fn pipeline_for_linear(st: &MetalState, dtype: DType) -> Option<(Id, usize)> {
            match dtype {
                DType::F32 => Some((st.pipe_linear_f32, 4)),
                DType::F16 => Some((st.pipe_linear_f16, 2)),
                DType::BF16 => Some((st.pipe_linear_bf16, 2)),
                _ => None,
            }
        }

        #[allow(clippy::too_many_arguments)]
        fn dispatch_linear_vram_locked(
            &self,
            st: &MetalState,
            a_buf: Id,
            b_buf: Id,
            bias_buf: Option<(Id, usize)>,
            a_bases: &[u32],
            b_bases: &[u32],
            batches: usize,
            m: usize,
            n: usize,
            k: usize,
            act: u32,
            dtype: DType,
        ) -> MoraResult<Option<Arc<MetalVramBuffer>>> {
            let Some((pipe, elem_bytes)) = Self::pipeline_for_linear(st, dtype) else {
                return Ok(None);
            };
            let (h_bias, has_bias, bias_len) = match bias_buf {
                Some((h, len)) => (h, 1u32, len.max(1) as u32),
                None => (st.dummy_buf, 0u32, 1u32),
            };
            let params: [u32; 7] = [
                m as u32,
                n as u32,
                k as u32,
                has_bias,
                bias_len,
                act,
                batches as u32,
            ];
            let out_buf = self.alloc_owned_vram(st, batches * m * n * elem_bytes)?;
            let mut temps = Vec::with_capacity(3);
            let run = (|| -> MoraResult<()> {
                let h_p =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &params)?;
                let h_ab =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, a_bases)?;
                let h_bb =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, b_bases)?;
                if dtype == DType::F16 {
                    self.inner.counters.on_f16_dispatch();
                } else if dtype == DType::BF16 {
                    self.inner.counters.on_bf16_dispatch();
                }
                let grid = MtlSize {
                    width: n.max(1),
                    height: m.max(1),
                    depth: batches.max(1),
                };
                let tg = MtlSize {
                    width: n.clamp(1, 16),
                    height: m.clamp(1, 16),
                    depth: 1,
                };
                Self::dispatch_compute(
                    &self.inner.objc,
                    st.queue,
                    pipe,
                    &[a_buf, b_buf, h_bias, out_buf.buf, h_p, h_ab, h_bb],
                    grid,
                    tg,
                )
            })();
            Self::release_temps(&self.inner.objc, temps);
            run?;
            Ok(Some(out_buf))
        }

        #[allow(clippy::too_many_arguments)]
        fn dispatch_linear_batched_f32_host(
            &self,
            a: &[f32],
            b: &[f32],
            bias: &[f32],
            a_bases: &[u32],
            b_bases: &[u32],
            batches: usize,
            m: usize,
            n: usize,
            k: usize,
            act: u32,
            out_f32: &mut [f32],
        ) -> MoraResult<()> {
            let st = self.inner.state.lock();
            let mut temps = Vec::with_capacity(7);
            let run = (|| -> MoraResult<()> {
                let h_a = Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, a)?;
                let h_b = Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, b)?;
                self.inner.counters.on_h2d();
                self.inner.counters.on_h2d();
                let h_bias = if bias.is_empty() {
                    st.dummy_buf
                } else {
                    self.inner.counters.on_h2d();
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, bias)?
                };
                let h_out = Self::alloc_mtl_buf_len(
                    &self.inner.objc,
                    st.mtl_dev,
                    std::mem::size_of_val(out_f32),
                )?;
                temps.push(h_out);
                let params: [u32; 7] = [
                    m as u32,
                    n as u32,
                    k as u32,
                    u32::from(!bias.is_empty()),
                    bias.len().max(1) as u32,
                    act,
                    batches as u32,
                ];
                let h_p =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &params)?;
                let h_ab =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, a_bases)?;
                let h_bb =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, b_bases)?;
                let grid = MtlSize {
                    width: n.max(1),
                    height: m.max(1),
                    depth: batches.max(1),
                };
                let tg = MtlSize {
                    width: n.clamp(1, 16),
                    height: m.clamp(1, 16),
                    depth: 1,
                };
                Self::dispatch_compute(
                    &self.inner.objc,
                    st.queue,
                    st.pipe_linear_f32,
                    &[h_a, h_b, h_bias, h_out, h_p, h_ab, h_bb],
                    grid,
                    tg,
                )?;
                Self::read_mtl_buf(&self.inner.objc, h_out, out_f32)?;
                self.inner.counters.on_d2h();
                Ok(())
            })();
            Self::release_temps(&self.inner.objc, temps);
            run
        }

        #[allow(clippy::too_many_arguments)]
        fn dispatch_linear_batched_half_host(
            &self,
            a: &[f64],
            b: &[f64],
            bias: &[f64],
            a_bases: &[u32],
            b_bases: &[u32],
            batches: usize,
            m: usize,
            n: usize,
            k: usize,
            act: u32,
            is_bf16: bool,
            out: &mut [f64],
        ) -> MoraResult<()> {
            let conv_to = |v: f64| {
                if is_bf16 {
                    f64_to_bf16_bits(v)
                } else {
                    f64_to_f16_bits(v)
                }
            };
            let a_h: Vec<u16> = a.iter().map(|&v| conv_to(v)).collect();
            let b_h: Vec<u16> = b.iter().map(|&v| conv_to(v)).collect();
            let bias_h: Vec<u16> = bias.iter().map(|&v| conv_to(v)).collect();
            let mut out_h = vec![0u16; batches * m * n];

            let st = self.inner.state.lock();
            let mut temps = Vec::with_capacity(7);
            let run = (|| -> MoraResult<()> {
                let h_a = Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &a_h)?;
                let h_b = Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &b_h)?;
                self.inner.counters.on_h2d();
                self.inner.counters.on_h2d();
                let h_bias = if bias_h.is_empty() {
                    st.dummy_buf
                } else {
                    self.inner.counters.on_h2d();
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &bias_h)?
                };
                let h_out = Self::alloc_mtl_buf_len(
                    &self.inner.objc,
                    st.mtl_dev,
                    std::mem::size_of_val(&out_h[..]),
                )?;
                temps.push(h_out);
                let params: [u32; 7] = [
                    m as u32,
                    n as u32,
                    k as u32,
                    u32::from(!bias_h.is_empty()),
                    bias_h.len().max(1) as u32,
                    act,
                    batches as u32,
                ];
                let h_p =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &params)?;
                let h_ab =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, a_bases)?;
                let h_bb =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, b_bases)?;
                let pipe = if is_bf16 {
                    self.inner.counters.on_bf16_dispatch();
                    st.pipe_linear_bf16
                } else {
                    self.inner.counters.on_f16_dispatch();
                    st.pipe_linear_f16
                };
                let grid = MtlSize {
                    width: n.max(1),
                    height: m.max(1),
                    depth: batches.max(1),
                };
                let tg = MtlSize {
                    width: n.clamp(1, 16),
                    height: m.clamp(1, 16),
                    depth: 1,
                };
                Self::dispatch_compute(
                    &self.inner.objc,
                    st.queue,
                    pipe,
                    &[h_a, h_b, h_bias, h_out, h_p, h_ab, h_bb],
                    grid,
                    tg,
                )?;
                Self::read_mtl_buf(&self.inner.objc, h_out, &mut out_h)?;
                self.inner.counters.on_d2h();
                Ok(())
            })();
            Self::release_temps(&self.inner.objc, temps);
            run?;
            for (d, &bits) in out.iter_mut().zip(out_h.iter()) {
                *d = if is_bf16 {
                    bf16_bits_to_f64(bits)
                } else {
                    f16_bits_to_f64(bits)
                };
            }
            Ok(())
        }
    }

    impl DeviceBackend for MetalDeviceBackend {
        fn kind(&self) -> Device {
            self.inner.device.clone()
        }

        fn name(&self) -> &str {
            "metal"
        }

        fn simd_isa(&self) -> &'static str {
            "msl-f32/f16/bf16"
        }

        fn worker_threads(&self) -> usize {
            self.info.multiprocessors as usize
        }

        fn is_available(&self) -> bool {
            true
        }

        fn supports_vram_residency(&self) -> bool {
            true
        }

        fn alloc_vram_buffer(
            &self,
            byte_len: usize,
        ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
            let st = self.inner.state.lock();
            let buf = self.alloc_owned_vram(&st, byte_len)?;
            Ok(Some(buf))
        }

        fn vram_telemetry(&self) -> VramTelemetry {
            self.inner.counters.snapshot()
        }

        fn run_matmul_vram(
            &self,
            a: &dyn DeviceVramBuffer,
            b: &dyn DeviceVramBuffer,
            a_shape: &[usize],
            b_shape: &[usize],
            dtype: DType,
        ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
            if a_shape.len() < 2
                || b_shape.len() < 2
                || !matches!(dtype, DType::F32 | DType::F16 | DType::BF16)
            {
                return Ok(None);
            }
            let m = a_shape[a_shape.len() - 2];
            let k = a_shape[a_shape.len() - 1];
            let k2 = b_shape[b_shape.len() - 2];
            let n = b_shape[b_shape.len() - 1];
            if k != k2 {
                return Err(MoraError::Runtime(format!(
                    "Metal MatMul inner dim mismatch: {k} != {k2}"
                )));
            }
            let a_batch = &a_shape[..a_shape.len() - 2];
            let b_batch = &b_shape[..b_shape.len() - 2];
            let out_batch = broadcast_batch_shapes(a_batch, b_batch)?;
            let batches = out_batch.iter().copied().product::<usize>().max(1);
            if batches * m * n == 0 || k == 0 {
                return Ok(None);
            }
            let a_bases: Vec<u32> = (0..batches)
                .map(|bi| (map_broadcast_batch(bi, &out_batch, a_batch) * m * k) as u32)
                .collect();
            let b_bases: Vec<u32> = (0..batches)
                .map(|bi| (map_broadcast_batch(bi, &out_batch, b_batch) * k * n) as u32)
                .collect();

            let st = self.inner.state.lock();
            let out = self.dispatch_linear_vram_locked(
                &st,
                a.raw_handle() as Id,
                b.raw_handle() as Id,
                None,
                &a_bases,
                &b_bases,
                batches,
                m,
                n,
                k,
                0,
                dtype,
            )?;
            if let Some(buf) = out {
                self.inner.counters.on_vram_hit();
                return Ok(Some(buf));
            }
            Ok(None)
        }

        #[allow(clippy::too_many_arguments)]
        fn run_fused_linear_act_vram(
            &self,
            x: &dyn DeviceVramBuffer,
            w: &dyn DeviceVramBuffer,
            bias: Option<(&dyn DeviceVramBuffer, &[usize])>,
            act: FusedAct,
            m_total: usize,
            k: usize,
            n: usize,
            _out_shape: &[usize],
            dtype: DType,
        ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
            if m_total * n == 0 || !matches!(dtype, DType::F32 | DType::F16 | DType::BF16) {
                return Ok(None);
            }
            let bias_spec = match bias {
                None => None,
                Some((vb, shape)) => {
                    let blen: usize = shape.iter().copied().product();
                    if blen == n || blen == 1 {
                        Some((vb.raw_handle() as Id, blen))
                    } else {
                        return Ok(None);
                    }
                }
            };
            let st = self.inner.state.lock();
            let out = self.dispatch_linear_vram_locked(
                &st,
                x.raw_handle() as Id,
                w.raw_handle() as Id,
                bias_spec,
                &[0u32],
                &[0u32],
                1,
                m_total,
                n,
                k,
                act_code(act),
                dtype,
            )?;
            if let Some(buf) = out {
                self.inner.counters.on_vram_hit();
                return Ok(Some(buf));
            }
            Ok(None)
        }

        #[allow(clippy::too_many_arguments)]
        fn run_fused_rms_norm_linear_vram(
            &self,
            x: &dyn DeviceVramBuffer,
            eps: f64,
            gamma: Option<(&dyn DeviceVramBuffer, &[usize])>,
            w: &dyn DeviceVramBuffer,
            bias: Option<(&dyn DeviceVramBuffer, &[usize])>,
            act: FusedAct,
            m_total: usize,
            k: usize,
            n: usize,
            _x_shape: &[usize],
            _out_shape: &[usize],
            dtype: DType,
        ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
            if dtype != DType::F32 || m_total == 0 || k == 0 || n == 0 {
                return Ok(None);
            }
            let (h_gamma, has_gamma, gamma_len) = match gamma {
                None => (ptr::null_mut(), 0u32, 1u32),
                Some((vg, gshape)) => {
                    let glen: usize = gshape.iter().copied().product();
                    if glen == k || glen == 1 {
                        (vg.raw_handle() as Id, 1u32, glen as u32)
                    } else {
                        return Ok(None);
                    }
                }
            };
            let bias_spec = match bias {
                None => None,
                Some((vb, bshape)) => {
                    let blen: usize = bshape.iter().copied().product();
                    if blen == n || blen == 1 {
                        Some((vb.raw_handle() as Id, blen))
                    } else {
                        return Ok(None);
                    }
                }
            };

            let st = self.inner.state.lock();
            let mut temps = Vec::with_capacity(2);
            let run = (|| -> MoraResult<Arc<MetalVramBuffer>> {
                let norm_buf =
                    Self::alloc_mtl_buf_len(&self.inner.objc, st.mtl_dev, m_total * k * 4)?;
                temps.push(norm_buf);
                let rms_params: [u32; 5] = [
                    m_total as u32,
                    k as u32,
                    has_gamma,
                    gamma_len,
                    (eps as f32).to_bits(),
                ];
                let h_rp =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &rms_params)?;
                let g_buf = if has_gamma != 0 {
                    h_gamma
                } else {
                    st.dummy_buf
                };
                Self::dispatch_compute(
                    &self.inner.objc,
                    st.queue,
                    st.pipe_rms_norm_f32,
                    &[x.raw_handle() as Id, g_buf, norm_buf, h_rp],
                    MtlSize {
                        width: m_total.max(1),
                        height: 1,
                        depth: 1,
                    },
                    MtlSize {
                        width: m_total.clamp(1, 64),
                        height: 1,
                        depth: 1,
                    },
                )?;
                let Some(out_buf) = self.dispatch_linear_vram_locked(
                    &st,
                    norm_buf,
                    w.raw_handle() as Id,
                    bias_spec,
                    &[0u32],
                    &[0u32],
                    1,
                    m_total,
                    n,
                    k,
                    act_code(act),
                    DType::F32,
                )?
                else {
                    return Err(MoraError::Runtime(
                        "Metal FusedRmsNormLinear linear stage returned None".to_string(),
                    ));
                };
                Ok(out_buf)
            })();
            Self::release_temps(&self.inner.objc, temps);
            let out_buf = run?;
            self.inner.counters.on_vram_hit();
            Ok(Some(out_buf))
        }

        #[allow(clippy::too_many_arguments)]
        fn run_fused_flash_attention_vram(
            &self,
            q: &dyn DeviceVramBuffer,
            k: &dyn DeviceVramBuffer,
            v: &dyn DeviceVramBuffer,
            mask: Option<(&dyn DeviceVramBuffer, &[usize])>,
            q_shape: &[usize],
            k_shape: &[usize],
            v_shape: &[usize],
            out_shape: &[usize],
            scale: f64,
            k_is_transposed: bool,
            dtype: DType,
        ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
            if dtype != DType::F32 || q_shape.len() < 2 || k_shape.len() < 2 || v_shape.len() < 2 {
                return Ok(None);
            }
            let s_q = q_shape[q_shape.len() - 2];
            let d_k = q_shape[q_shape.len() - 1];
            let (s_k, k_d) = if k_is_transposed {
                (k_shape[k_shape.len() - 1], k_shape[k_shape.len() - 2])
            } else {
                (k_shape[k_shape.len() - 2], k_shape[k_shape.len() - 1])
            };
            let d_v = v_shape[v_shape.len() - 1];
            if d_k != k_d || s_k != v_shape[v_shape.len() - 2] {
                return Err(MoraError::Runtime(format!(
                    "Metal FlashAttention shape mismatch: Q={q_shape:?}, K={k_shape:?}, V={v_shape:?}"
                )));
            }
            let batch_q = &q_shape[..q_shape.len() - 2];
            let batch_k = &k_shape[..k_shape.len() - 2];
            let batch_v = &v_shape[..v_shape.len() - 2];
            let batch_out = &out_shape[..out_shape.len() - 2];
            let batches = batch_out.iter().copied().product::<usize>().max(1);
            let out_numel = batches * s_q * d_v;
            if out_numel == 0 {
                return Ok(None);
            }

            let q_bases: Vec<u32> = (0..batches)
                .map(|b| (map_broadcast_batch(b, batch_out, batch_q) * s_q * d_k) as u32)
                .collect();
            let k_bases: Vec<u32> = (0..batches)
                .map(|b| (map_broadcast_batch(b, batch_out, batch_k) * s_k * d_k) as u32)
                .collect();
            let v_bases: Vec<u32> = (0..batches)
                .map(|b| (map_broadcast_batch(b, batch_out, batch_v) * s_k * d_v) as u32)
                .collect();
            let (h_mask_opt, mask_bases, mask_sq_s, mask_sk_s, has_mask) =
                if let Some((vm, mshape)) = mask {
                    let mrank = mshape.len();
                    let m_sk = *mshape.last().unwrap_or(&1);
                    let m_sq = if mrank >= 2 { mshape[mrank - 2] } else { 1 };
                    let mbatch = if mrank >= 2 {
                        &mshape[..mrank - 2]
                    } else {
                        &[]
                    };
                    let sq_s = if m_sq == 1 { 0 } else { m_sk as u32 };
                    let sk_s = if m_sk == 1 { 0 } else { 1u32 };
                    let mb: Vec<u32> = (0..batches)
                        .map(|b| (map_broadcast_batch(b, batch_out, mbatch) * m_sq * m_sk) as u32)
                        .collect();
                    (Some(vm.raw_handle() as Id), mb, sq_s, sk_s, 1u32)
                } else {
                    (None, vec![0u32; batches], 0u32, 0u32, 0u32)
                };

            let params: [u32; 10] = [
                batches as u32,
                s_q as u32,
                s_k as u32,
                d_k as u32,
                d_v as u32,
                u32::from(k_is_transposed),
                has_mask,
                mask_sq_s,
                mask_sk_s,
                (scale as f32).to_bits(),
            ];

            let st = self.inner.state.lock();
            let out_buf = self.alloc_owned_vram(&st, out_numel * 4)?;
            let mut temps = Vec::with_capacity(5);
            let run = (|| -> MoraResult<()> {
                let h_qb =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &q_bases)?;
                let h_kb =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &k_bases)?;
                let h_vb =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &v_bases)?;
                let h_mb =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &mask_bases)?;
                let h_p =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &params)?;
                Self::dispatch_compute(
                    &self.inner.objc,
                    st.queue,
                    st.pipe_flash_attn_f32,
                    &[
                        q.raw_handle() as Id,
                        k.raw_handle() as Id,
                        v.raw_handle() as Id,
                        h_mask_opt.unwrap_or(st.dummy_buf),
                        out_buf.buf,
                        h_qb,
                        h_kb,
                        h_vb,
                        h_mb,
                        h_p,
                    ],
                    MtlSize {
                        width: d_v.max(1),
                        height: s_q.max(1),
                        depth: batches.max(1),
                    },
                    MtlSize {
                        width: d_v.clamp(1, 16),
                        height: s_q.clamp(1, 16),
                        depth: 1,
                    },
                )
            })();
            Self::release_temps(&self.inner.objc, temps);
            run?;
            self.inner.counters.on_vram_hit();
            Ok(Some(out_buf))
        }

        #[allow(clippy::too_many_arguments)]
        fn run_fused_elementwise_vram(
            &self,
            inputs: &[&dyn DeviceVramBuffer],
            input_shapes: &[&[usize]],
            steps: &[FusedEwStep],
            out_shape: &[usize],
            out_numel: usize,
            dtype: DType,
        ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
            let total_slots = inputs.len() + steps.len();
            if dtype != DType::F32
                || out_numel == 0
                || steps.is_empty()
                || inputs.is_empty()
                || inputs.len() > 4
                || total_slots > 64
                || out_shape.len() > 16
            {
                return Ok(None);
            }
            let eff_out_shape: Vec<usize> = if out_shape.is_empty() {
                vec![1]
            } else {
                out_shape.to_vec()
            };
            let ndim = eff_out_shape.len();
            let out_shape_u32: Vec<u32> = eff_out_shape.iter().map(|&d| d as u32).collect();
            let mut all_strides: Vec<u32> = Vec::with_capacity(input_shapes.len() * ndim);
            for s in input_shapes {
                all_strides.extend(compute_broadcast_strides_u32(s, &eff_out_shape));
            }
            let (ops, arg_a, arg_b, imm_bits) = encode_ew_steps(steps);
            let mut meta = Vec::with_capacity(5 + steps.len() + inputs.len());
            meta.push(ndim as u32);
            meta.push(inputs.len() as u32);
            meta.push(steps.len() as u32);
            meta.push(out_numel as u32);
            meta.push(0u32); // packed_mode = 0 (direct MTLBuffer bindings)
            meta.extend_from_slice(&imm_bits);
            meta.resize(meta.len() + inputs.len(), 0);

            let st = self.inner.state.lock();
            let out_buf = self.alloc_owned_vram(&st, out_numel * 4)?;
            let mut temps = Vec::with_capacity(6);
            let run = (|| -> MoraResult<()> {
                let h_st = Self::upload_temp_slice(
                    &self.inner.objc,
                    st.mtl_dev,
                    &mut temps,
                    &all_strides,
                )?;
                let h_os = Self::upload_temp_slice(
                    &self.inner.objc,
                    st.mtl_dev,
                    &mut temps,
                    &out_shape_u32,
                )?;
                let h_op = Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &ops)?;
                let h_aa =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &arg_a)?;
                let h_ab =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &arg_b)?;
                let h_mt =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &meta)?;
                let in_h = |i: usize| inputs.get(i).map_or(st.dummy_buf, |b| b.raw_handle() as Id);
                Self::dispatch_compute(
                    &self.inner.objc,
                    st.queue,
                    st.pipe_fused_ew_f32,
                    &[
                        out_buf.buf,
                        in_h(0),
                        in_h(1),
                        in_h(2),
                        in_h(3),
                        h_st,
                        h_os,
                        h_op,
                        h_aa,
                        h_ab,
                        h_mt,
                    ],
                    MtlSize {
                        width: out_numel.max(1),
                        height: 1,
                        depth: 1,
                    },
                    MtlSize {
                        width: out_numel.clamp(1, 64),
                        height: 1,
                        depth: 1,
                    },
                )
            })();
            Self::release_temps(&self.inner.objc, temps);
            run?;
            self.inner.counters.on_vram_hit();
            Ok(Some(out_buf))
        }

        fn run_matmul_into(
            &self,
            a: &[f64],
            b: &[f64],
            a_shape: &[usize],
            b_shape: &[usize],
            dtype: DType,
            out: &mut Vec<f64>,
        ) -> MoraResult<()> {
            if a_shape.len() < 2 || b_shape.len() < 2 {
                simd_kernels::matmul_simd_into(a, b, a_shape, b_shape, out)?;
                if dtype != DType::F64 {
                    for v in out.iter_mut() {
                        *v = dtype.quantize_f64(*v);
                    }
                }
                return Ok(());
            }
            let m = a_shape[a_shape.len() - 2];
            let k = a_shape[a_shape.len() - 1];
            let k2 = b_shape[b_shape.len() - 2];
            let n = b_shape[b_shape.len() - 1];
            if k != k2 {
                return Err(MoraError::Runtime(format!(
                    "Metal MatMul inner dim mismatch: {k} != {k2}"
                )));
            }
            let a_batch = &a_shape[..a_shape.len() - 2];
            let b_batch = &b_shape[..b_shape.len() - 2];
            let out_batch = broadcast_batch_shapes(a_batch, b_batch)?;
            let batches = out_batch.iter().copied().product::<usize>().max(1);
            let out_numel = batches * m * n;
            out.resize(out_numel, 0.0);
            if out_numel == 0 || k == 0 {
                out.fill(0.0);
                return Ok(());
            }
            let a_bases: Vec<u32> = (0..batches)
                .map(|bi| (map_broadcast_batch(bi, &out_batch, a_batch) * m * k) as u32)
                .collect();
            let b_bases: Vec<u32> = (0..batches)
                .map(|bi| (map_broadcast_batch(bi, &out_batch, b_batch) * k * n) as u32)
                .collect();

            if matches!(dtype, DType::F16 | DType::BF16) {
                return self.dispatch_linear_batched_half_host(
                    a,
                    b,
                    &[],
                    &a_bases,
                    &b_bases,
                    batches,
                    m,
                    n,
                    k,
                    0,
                    dtype == DType::BF16,
                    out,
                );
            }

            let a32: Vec<f32> = a.iter().map(|&v| v as f32).collect();
            let b32: Vec<f32> = b.iter().map(|&v| v as f32).collect();
            let mut o32 = vec![0.0f32; out_numel];
            self.dispatch_linear_batched_f32_host(
                &a32,
                &b32,
                &[],
                &a_bases,
                &b_bases,
                batches,
                m,
                n,
                k,
                0,
                &mut o32,
            )?;
            for (d, &v) in out.iter_mut().zip(o32.iter()) {
                *d = dtype.quantize_f64(f64::from(v));
            }
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn run_fused_linear_act_into(
            &self,
            x: &[f64],
            w: &[f64],
            bias: Option<(&[f64], &[usize])>,
            act: FusedAct,
            m_total: usize,
            k: usize,
            n: usize,
            out_shape: &[usize],
            dtype: DType,
            out: &mut Vec<f64>,
        ) -> MoraResult<()> {
            out.resize(m_total * n, 0.0);
            if m_total * n == 0 {
                return Ok(());
            }
            let bias_slice: &[f64] = match bias {
                None => &[],
                Some((bv, _)) if bv.len() == n || bv.len() == 1 => bv,
                Some(_) => {
                    simd_kernels::fused_linear_act_into(
                        x, w, bias, act, m_total, k, n, out_shape, dtype, out,
                    );
                    return Ok(());
                }
            };
            if matches!(dtype, DType::F16 | DType::BF16) {
                return self.dispatch_linear_batched_half_host(
                    x,
                    w,
                    bias_slice,
                    &[0u32],
                    &[0u32],
                    1,
                    m_total,
                    n,
                    k,
                    act_code(act),
                    dtype == DType::BF16,
                    out,
                );
            }
            let bias32: Vec<f32> = bias_slice.iter().map(|&v| v as f32).collect();
            let x32: Vec<f32> = x.iter().map(|&v| v as f32).collect();
            let w32: Vec<f32> = w.iter().map(|&v| v as f32).collect();
            let mut o32 = vec![0.0f32; m_total * n];
            self.dispatch_linear_batched_f32_host(
                &x32,
                &w32,
                &bias32,
                &[0u32],
                &[0u32],
                1,
                m_total,
                n,
                k,
                act_code(act),
                &mut o32,
            )?;
            for (d, &v) in out.iter_mut().zip(o32.iter()) {
                *d = dtype.quantize_f64(f64::from(v));
            }
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn run_fused_rms_norm_linear_into(
            &self,
            x: &[f64],
            eps: f64,
            gamma: Option<(&[f64], &[usize])>,
            w: &[f64],
            bias: Option<(&[f64], &[usize])>,
            act: FusedAct,
            m_total: usize,
            k: usize,
            n: usize,
            x_shape: &[usize],
            out_shape: &[usize],
            dtype: DType,
            out: &mut Vec<f64>,
        ) -> MoraResult<()> {
            out.resize(m_total * n, 0.0);
            if m_total == 0 || k == 0 || n == 0 {
                return Ok(());
            }
            let gamma_f32: Vec<f32> = match gamma {
                None => Vec::new(),
                Some((gv, _)) if gv.len() == k || gv.len() == 1 => {
                    gv.iter().map(|&v| v as f32).collect()
                }
                Some(_) => {
                    simd_kernels::fused_rms_norm_linear_into(
                        x, eps, gamma, w, bias, act, m_total, k, n, x_shape, out_shape, dtype, out,
                    );
                    return Ok(());
                }
            };
            let bias_f32: Vec<f32> = match bias {
                None => Vec::new(),
                Some((bv, _)) if bv.len() == n || bv.len() == 1 => {
                    bv.iter().map(|&v| v as f32).collect()
                }
                Some(_) => {
                    simd_kernels::fused_rms_norm_linear_into(
                        x, eps, gamma, w, bias, act, m_total, k, n, x_shape, out_shape, dtype, out,
                    );
                    return Ok(());
                }
            };
            let x32: Vec<f32> = x.iter().map(|&v| v as f32).collect();
            let w32: Vec<f32> = w.iter().map(|&v| v as f32).collect();
            let mut o32 = vec![0.0f32; m_total * n];

            let st = self.inner.state.lock();
            let mut temps = Vec::with_capacity(9);
            let run = (|| -> MoraResult<()> {
                let h_x = Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &x32)?;
                self.inner.counters.on_h2d();
                let h_g = if gamma_f32.is_empty() {
                    st.dummy_buf
                } else {
                    self.inner.counters.on_h2d();
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &gamma_f32)?
                };
                let h_norm =
                    Self::alloc_mtl_buf_len(&self.inner.objc, st.mtl_dev, m_total * k * 4)?;
                temps.push(h_norm);
                let rms_params: [u32; 5] = [
                    m_total as u32,
                    k as u32,
                    u32::from(!gamma_f32.is_empty()),
                    gamma_f32.len().max(1) as u32,
                    (eps as f32).to_bits(),
                ];
                let h_rp =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &rms_params)?;
                Self::dispatch_compute(
                    &self.inner.objc,
                    st.queue,
                    st.pipe_rms_norm_f32,
                    &[h_x, h_g, h_norm, h_rp],
                    MtlSize {
                        width: m_total.max(1),
                        height: 1,
                        depth: 1,
                    },
                    MtlSize {
                        width: m_total.clamp(1, 64),
                        height: 1,
                        depth: 1,
                    },
                )?;

                let h_w = Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &w32)?;
                self.inner.counters.on_h2d();
                let h_b = if bias_f32.is_empty() {
                    st.dummy_buf
                } else {
                    self.inner.counters.on_h2d();
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &bias_f32)?
                };
                let h_out = Self::alloc_mtl_buf_len(&self.inner.objc, st.mtl_dev, m_total * n * 4)?;
                temps.push(h_out);
                let lin_params: [u32; 7] = [
                    m_total as u32,
                    n as u32,
                    k as u32,
                    u32::from(!bias_f32.is_empty()),
                    bias_f32.len().max(1) as u32,
                    act_code(act),
                    1u32,
                ];
                let h_lp =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &lin_params)?;
                let h_z =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &[0u32])?;
                Self::dispatch_compute(
                    &self.inner.objc,
                    st.queue,
                    st.pipe_linear_f32,
                    &[h_norm, h_w, h_b, h_out, h_lp, h_z, h_z],
                    MtlSize {
                        width: n.max(1),
                        height: m_total.max(1),
                        depth: 1,
                    },
                    MtlSize {
                        width: n.clamp(1, 16),
                        height: m_total.clamp(1, 16),
                        depth: 1,
                    },
                )?;
                Self::read_mtl_buf(&self.inner.objc, h_out, &mut o32)?;
                self.inner.counters.on_d2h();
                Ok(())
            })();
            Self::release_temps(&self.inner.objc, temps);
            run?;
            for (d, &v) in out.iter_mut().zip(o32.iter()) {
                *d = dtype.quantize_f64(f64::from(v));
            }
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn run_fused_flash_attention_into(
            &self,
            q: &[f64],
            k: &[f64],
            v: &[f64],
            mask: Option<(&[f64], &[usize])>,
            q_shape: &[usize],
            k_shape: &[usize],
            v_shape: &[usize],
            out_shape: &[usize],
            scale: f64,
            k_is_transposed: bool,
            dtype: DType,
            out: &mut Vec<f64>,
        ) -> MoraResult<()> {
            if q_shape.len() < 2 || k_shape.len() < 2 || v_shape.len() < 2 {
                return simd_kernels::fused_flash_attention_into(
                    q,
                    k,
                    v,
                    mask,
                    q_shape,
                    k_shape,
                    v_shape,
                    out_shape,
                    scale,
                    k_is_transposed,
                    dtype,
                    out,
                );
            }
            let s_q = q_shape[q_shape.len() - 2];
            let d_k = q_shape[q_shape.len() - 1];
            let (s_k, k_d) = if k_is_transposed {
                (k_shape[k_shape.len() - 1], k_shape[k_shape.len() - 2])
            } else {
                (k_shape[k_shape.len() - 2], k_shape[k_shape.len() - 1])
            };
            let d_v = v_shape[v_shape.len() - 1];
            if d_k != k_d || s_k != v_shape[v_shape.len() - 2] {
                return Err(MoraError::Runtime(format!(
                    "Metal FlashAttention shape mismatch: Q={q_shape:?}, K={k_shape:?}, V={v_shape:?}"
                )));
            }
            let batch_q = &q_shape[..q_shape.len() - 2];
            let batch_k = &k_shape[..k_shape.len() - 2];
            let batch_v = &v_shape[..v_shape.len() - 2];
            let batch_out = &out_shape[..out_shape.len() - 2];
            let batches = batch_out.iter().copied().product::<usize>().max(1);
            let out_numel = batches * s_q * d_v;
            out.resize(out_numel, 0.0);
            if out_numel == 0 {
                return Ok(());
            }

            let q_bases: Vec<u32> = (0..batches)
                .map(|b| (map_broadcast_batch(b, batch_out, batch_q) * s_q * d_k) as u32)
                .collect();
            let k_bases: Vec<u32> = (0..batches)
                .map(|b| (map_broadcast_batch(b, batch_out, batch_k) * s_k * d_k) as u32)
                .collect();
            let v_bases: Vec<u32> = (0..batches)
                .map(|b| (map_broadcast_batch(b, batch_out, batch_v) * s_k * d_v) as u32)
                .collect();
            let (mask_f32, mask_bases, mask_sq_s, mask_sk_s, has_mask) =
                if let Some((mv, mshape)) = mask {
                    let mrank = mshape.len();
                    let m_sk = *mshape.last().unwrap_or(&1);
                    let m_sq = if mrank >= 2 { mshape[mrank - 2] } else { 1 };
                    let mbatch = if mrank >= 2 {
                        &mshape[..mrank - 2]
                    } else {
                        &[]
                    };
                    let sq_s = if m_sq == 1 { 0 } else { m_sk as u32 };
                    let sk_s = if m_sk == 1 { 0 } else { 1u32 };
                    let mb: Vec<u32> = (0..batches)
                        .map(|b| (map_broadcast_batch(b, batch_out, mbatch) * m_sq * m_sk) as u32)
                        .collect();
                    let mf: Vec<f32> = mv.iter().map(|&x| x as f32).collect();
                    (mf, mb, sq_s, sk_s, 1u32)
                } else {
                    (Vec::new(), vec![0u32; batches], 0u32, 0u32, 0u32)
                };

            let q32: Vec<f32> = q.iter().map(|&x| x as f32).collect();
            let k32: Vec<f32> = k.iter().map(|&x| x as f32).collect();
            let v32: Vec<f32> = v.iter().map(|&x| x as f32).collect();
            let mut o32 = vec![0.0f32; out_numel];
            let params: [u32; 10] = [
                batches as u32,
                s_q as u32,
                s_k as u32,
                d_k as u32,
                d_v as u32,
                u32::from(k_is_transposed),
                has_mask,
                mask_sq_s,
                mask_sk_s,
                (scale as f32).to_bits(),
            ];

            let st = self.inner.state.lock();
            let mut temps = Vec::with_capacity(10);
            let run = (|| -> MoraResult<()> {
                let h_q = Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &q32)?;
                let h_k = Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &k32)?;
                let h_v = Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &v32)?;
                self.inner.counters.on_h2d();
                self.inner.counters.on_h2d();
                self.inner.counters.on_h2d();
                let h_m = if mask_f32.is_empty() {
                    st.dummy_buf
                } else {
                    self.inner.counters.on_h2d();
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &mask_f32)?
                };
                let h_out = Self::alloc_mtl_buf_len(&self.inner.objc, st.mtl_dev, out_numel * 4)?;
                temps.push(h_out);
                let h_qb =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &q_bases)?;
                let h_kb =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &k_bases)?;
                let h_vb =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &v_bases)?;
                let h_mb =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &mask_bases)?;
                let h_p =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &params)?;
                Self::dispatch_compute(
                    &self.inner.objc,
                    st.queue,
                    st.pipe_flash_attn_f32,
                    &[h_q, h_k, h_v, h_m, h_out, h_qb, h_kb, h_vb, h_mb, h_p],
                    MtlSize {
                        width: d_v.max(1),
                        height: s_q.max(1),
                        depth: batches.max(1),
                    },
                    MtlSize {
                        width: d_v.clamp(1, 16),
                        height: s_q.clamp(1, 16),
                        depth: 1,
                    },
                )?;
                Self::read_mtl_buf(&self.inner.objc, h_out, &mut o32)?;
                self.inner.counters.on_d2h();
                Ok(())
            })();
            Self::release_temps(&self.inner.objc, temps);
            run?;
            for (d, &val) in out.iter_mut().zip(o32.iter()) {
                *d = dtype.quantize_f64(f64::from(val));
            }
            Ok(())
        }

        #[allow(clippy::too_many_arguments)]
        fn run_fused_elementwise_into(
            &self,
            input_flats: &[&[f64]],
            input_shapes: &[&[usize]],
            steps: &[FusedEwStep],
            out_shape: &[usize],
            out_numel: usize,
            dtype: DType,
            out: &mut Vec<f64>,
        ) -> MoraResult<()> {
            let total_slots = input_flats.len() + steps.len();
            if out_numel == 0
                || steps.is_empty()
                || input_flats.is_empty()
                || total_slots > 64
                || out_shape.len() > 16
            {
                return simd_kernels::fused_elementwise_into(
                    input_flats,
                    input_shapes,
                    steps,
                    out_shape,
                    out_numel,
                    dtype,
                    out,
                );
            }
            out.resize(out_numel, 0.0);
            let eff_out_shape: Vec<usize> = if out_shape.is_empty() {
                vec![1]
            } else {
                out_shape.to_vec()
            };
            let ndim = eff_out_shape.len();
            let out_shape_u32: Vec<u32> = eff_out_shape.iter().map(|&d| d as u32).collect();
            let mut all_strides: Vec<u32> = Vec::with_capacity(input_shapes.len() * ndim);
            for s in input_shapes {
                all_strides.extend(compute_broadcast_strides_u32(s, &eff_out_shape));
            }
            let (ops, arg_a, arg_b, imm_bits) = encode_ew_steps(steps);
            let mut packed_in: Vec<f32> = Vec::new();
            let mut in_offsets: Vec<u32> = Vec::with_capacity(input_flats.len());
            for flat in input_flats {
                in_offsets.push(packed_in.len() as u32);
                packed_in.extend(flat.iter().map(|&v| v as f32));
            }
            let mut meta = Vec::with_capacity(5 + steps.len() + input_flats.len());
            meta.push(ndim as u32);
            meta.push(input_flats.len() as u32);
            meta.push(steps.len() as u32);
            meta.push(out_numel as u32);
            meta.push(1u32); // packed_mode = 1
            meta.extend_from_slice(&imm_bits);
            meta.extend_from_slice(&in_offsets);

            let mut o32 = vec![0.0f32; out_numel];
            let st = self.inner.state.lock();
            let mut temps = Vec::with_capacity(8);
            let run = (|| -> MoraResult<()> {
                let h_out = Self::alloc_mtl_buf_len(&self.inner.objc, st.mtl_dev, out_numel * 4)?;
                temps.push(h_out);
                let h_in0 =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &packed_in)?;
                self.inner.counters.on_h2d();
                let h_st = Self::upload_temp_slice(
                    &self.inner.objc,
                    st.mtl_dev,
                    &mut temps,
                    &all_strides,
                )?;
                let h_os = Self::upload_temp_slice(
                    &self.inner.objc,
                    st.mtl_dev,
                    &mut temps,
                    &out_shape_u32,
                )?;
                let h_op = Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &ops)?;
                let h_aa =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &arg_a)?;
                let h_ab =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &arg_b)?;
                let h_mt =
                    Self::upload_temp_slice(&self.inner.objc, st.mtl_dev, &mut temps, &meta)?;
                Self::dispatch_compute(
                    &self.inner.objc,
                    st.queue,
                    st.pipe_fused_ew_f32,
                    &[
                        h_out,
                        h_in0,
                        st.dummy_buf,
                        st.dummy_buf,
                        st.dummy_buf,
                        h_st,
                        h_os,
                        h_op,
                        h_aa,
                        h_ab,
                        h_mt,
                    ],
                    MtlSize {
                        width: out_numel.max(1),
                        height: 1,
                        depth: 1,
                    },
                    MtlSize {
                        width: out_numel.clamp(1, 64),
                        height: 1,
                        depth: 1,
                    },
                )?;
                Self::read_mtl_buf(&self.inner.objc, h_out, &mut o32)?;
                self.inner.counters.on_d2h();
                Ok(())
            })();
            Self::release_temps(&self.inner.objc, temps);
            run?;
            for (d, &val) in out.iter_mut().zip(o32.iter()) {
                *d = dtype.quantize_f64(f64::from(val));
            }
            Ok(())
        }

        fn launch_custom_kernel(
            &self,
            kernel: &KernelProgram,
            grid: u32,
            block: u32,
            args: &[KernelLaunchArg],
        ) -> MoraResult<bool> {
            // Metal Shading Language does not support 64-bit IEEE float compute buffers;
            // fall back cleanly to the bounds-checked CPU executor for `f64` kernels.
            if kernel.uses_f64() {
                return Ok(false);
            }
            let hash = kernel.content_hash();
            let mut st = self.inner.state.lock();
            let pipeline = if let Some(&p) = st.custom_pipes.get(&hash) {
                p
            } else {
                let msl_src = emit_msl(kernel);
                // SAFETY: Compiles generated MSL source on `st.mtl_dev` via `newLibraryWithSource:options:error:`
                // and extracts the named compute pipeline state, releasing temporary ObjC objects immediately.
                let compiled = unsafe {
                    let s_new_lib = (self.inner.objc.sel)(
                        b"newLibraryWithSource:options:error:\0".as_ptr() as *const c_char,
                    );
                    let ns_src = self.inner.objc.make_nsstring(&msl_src);
                    let mut err: Id = ptr::null_mut();
                    let lib = (self.inner.objc.msg_new_lib)(
                        st.mtl_dev,
                        s_new_lib,
                        ns_src,
                        ptr::null_mut(),
                        &raw mut err,
                    );
                    self.inner.objc.release(ns_src);
                    if lib.is_null() {
                        return Err(MoraError::Runtime(format!(
                            "Metal MSL compilation failed for custom kernel '{}'",
                            kernel.name
                        )));
                    }
                    let pipe = build_pipeline(&self.inner.objc, st.mtl_dev, lib, &kernel.name);
                    self.inner.objc.release(lib);
                    pipe.ok_or_else(|| {
                        MoraError::Runtime(format!(
                            "Metal pipeline creation failed for custom kernel '{}'",
                            kernel.name
                        ))
                    })?
                };
                st.custom_pipes.insert(hash, compiled);
                compiled
            };

            let mut buffers: Vec<Id> = Vec::with_capacity(args.len() + 1);
            for arg in args {
                if let KernelLaunchArg::VramBuffer { handle, .. } = arg {
                    let ptr_id = *handle as Id;
                    if ptr_id.is_null() {
                        return Ok(false);
                    }
                    buffers.push(ptr_id);
                }
            }
            let scalar_words = pack_metal_custom_kernel_scalars(args);
            let mut temps: Vec<Id> = Vec::new();
            let run = (|| -> MoraResult<()> {
                if !scalar_words.is_empty() {
                    let h_scalars = Self::upload_temp_slice(
                        &self.inner.objc,
                        st.mtl_dev,
                        &mut temps,
                        &scalar_words,
                    )?;
                    buffers.push(h_scalars);
                }
                let total_grid = MtlSize {
                    width: (grid.max(1) as usize).saturating_mul(block.max(1) as usize),
                    height: 1,
                    depth: 1,
                };
                let tg = MtlSize {
                    width: block.max(1) as usize,
                    height: 1,
                    depth: 1,
                };
                Self::dispatch_compute(
                    &self.inner.objc,
                    st.queue,
                    pipeline,
                    &buffers,
                    total_grid,
                    tg,
                )?;
                self.inner.counters.on_vram_hit();
                Ok(())
            })();
            Self::release_temps(&self.inner.objc, temps);
            run?;
            Ok(true)
        }
    }

    fn build_pipeline(objc: &ObjC, mtl_dev: Id, lib: Id, fn_name: &str) -> Option<Id> {
        // SAFETY: Queries `lib` for `fn_name` and compiles its `MTLComputePipelineState`.
        unsafe {
            let s_new_fn = (objc.sel)(b"newFunctionWithName:\0".as_ptr() as *const c_char);
            let s_new_pipe = (objc.sel)(
                b"newComputePipelineStateWithFunction:error:\0".as_ptr() as *const c_char
            );
            let ns_fn = objc.make_nsstring(fn_name);
            let func = (objc.msg_new_fn)(lib, s_new_fn, ns_fn);
            objc.release(ns_fn);
            if func.is_null() {
                return None;
            }
            let mut pipe_err: Id = ptr::null_mut();
            let pipe = (objc.msg_new_pipe)(mtl_dev, s_new_pipe, func, &raw mut pipe_err);
            objc.release(func);
            if pipe.is_null() { None } else { Some(pipe) }
        }
    }

    pub(super) fn probe() -> Vec<HardwareBackendEntry> {
        let Some(objc) = ObjC::load() else {
            return Vec::new();
        };
        // SAFETY: Obtains the default Metal device and compiles Mora's 6 MSL kernels at runtime.
        unsafe {
            let mtl_dev = (objc.create_default_device)();
            if mtl_dev.is_null() {
                return Vec::new();
            }
            let s_name = (objc.sel)(b"name\0".as_ptr() as *const c_char);
            let s_utf8 = (objc.sel)(b"UTF8String\0".as_ptr() as *const c_char);
            let s_vram = (objc.sel)(b"recommendedMaxWorkingSetSize\0".as_ptr() as *const c_char);
            let s_queue = (objc.sel)(b"newCommandQueue\0".as_ptr() as *const c_char);
            let s_new_lib =
                (objc.sel)(b"newLibraryWithSource:options:error:\0".as_ptr() as *const c_char);

            let ns_name = (objc.msg0)(mtl_dev, s_name);
            let cname = if !ns_name.is_null() {
                let p = (objc.msg_cstr)(ns_name, s_utf8);
                if !p.is_null() {
                    CStr::from_ptr(p).to_string_lossy().into_owned()
                } else {
                    "Apple Metal GPU".to_string()
                }
            } else {
                "Apple Metal GPU".to_string()
            };
            let vram = (objc.msg_u64)(mtl_dev, s_vram);

            let queue = (objc.msg0)(mtl_dev, s_queue);
            let ns_src = objc.make_nsstring(MORA_METAL_MSL);
            let mut err: Id = ptr::null_mut();
            let lib = (objc.msg_new_lib)(mtl_dev, s_new_lib, ns_src, ptr::null_mut(), &raw mut err);
            objc.release(ns_src);
            if queue.is_null() || lib.is_null() {
                objc.release(lib);
                objc.release(queue);
                objc.release(mtl_dev);
                return Vec::new();
            }

            let pipes = (
                build_pipeline(&objc, mtl_dev, lib, "mora_fused_linear_f32"),
                build_pipeline(&objc, mtl_dev, lib, "mora_fused_linear_f16"),
                build_pipeline(&objc, mtl_dev, lib, "mora_fused_linear_bf16"),
                build_pipeline(&objc, mtl_dev, lib, "mora_rms_norm_f32"),
                build_pipeline(&objc, mtl_dev, lib, "mora_fused_flash_attention_f32"),
                build_pipeline(&objc, mtl_dev, lib, "mora_fused_elementwise_f32"),
            );
            objc.release(lib);

            let (
                Some(pipe_linear_f32),
                Some(pipe_linear_f16),
                Some(pipe_linear_bf16),
                Some(pipe_rms_norm_f32),
                Some(pipe_flash_attn_f32),
                Some(pipe_fused_ew_f32),
            ) = pipes
            else {
                if let Some(p) = pipes.0 {
                    objc.release(p);
                }
                if let Some(p) = pipes.1 {
                    objc.release(p);
                }
                if let Some(p) = pipes.2 {
                    objc.release(p);
                }
                if let Some(p) = pipes.3 {
                    objc.release(p);
                }
                if let Some(p) = pipes.4 {
                    objc.release(p);
                }
                if let Some(p) = pipes.5 {
                    objc.release(p);
                }
                objc.release(queue);
                objc.release(mtl_dev);
                return Vec::new();
            };

            let Ok(dummy_buf) = MetalDeviceBackend::alloc_mtl_buf_len(&objc, mtl_dev, 16) else {
                objc.release(pipe_fused_ew_f32);
                objc.release(pipe_flash_attn_f32);
                objc.release(pipe_rms_norm_f32);
                objc.release(pipe_linear_bf16);
                objc.release(pipe_linear_f16);
                objc.release(pipe_linear_f32);
                objc.release(queue);
                objc.release(mtl_dev);
                return Vec::new();
            };

            let device = Device::Metal(0);
            let info = GpuDeviceInfo {
                device: device.clone(),
                driver: "metal".to_string(),
                name: cname,
                total_vram_bytes: vram,
                compute_capability: "metal-msl-f32/f16/bf16".to_string(),
                multiprocessors: 1,
                hardware_accelerated: true,
                driver_version: "Metal.framework".to_string(),
            };
            let inner = Arc::new(MetalDeviceInner {
                device,
                objc: Arc::clone(&objc),
                state: Mutex::new(MetalState {
                    mtl_dev,
                    queue,
                    pipe_linear_f32,
                    pipe_linear_f16,
                    pipe_linear_bf16,
                    pipe_rms_norm_f32,
                    pipe_flash_attn_f32,
                    pipe_fused_ew_f32,
                    dummy_buf,
                    custom_pipes: HashMap::new(),
                }),
                counters: VramCounters::default(),
            });
            let backend = Arc::new(MetalDeviceBackend {
                inner,
                info: info.clone(),
            });
            vec![HardwareBackendEntry { info, backend }]
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn probe_metal_backends() -> Vec<HardwareBackendEntry> {
    macos_impl::probe()
}

#[cfg(test)]
mod tests {
    use super::{MORA_METAL_MSL, pack_metal_custom_kernel_scalars};
    use crate::common::kernel_ir::{KernelParam, KernelParamType, KernelProgram};
    use crate::runtime::gpu::kernel_compiler::{KernelLaunchArg, emit_msl};

    #[test]
    fn metal_msl_defines_all_six_kernels_and_balanced_braces() {
        for kernel in [
            "kernel void mora_fused_linear_f32(",
            "kernel void mora_fused_linear_f16(",
            "kernel void mora_fused_linear_bf16(",
            "kernel void mora_rms_norm_f32(",
            "kernel void mora_fused_flash_attention_f32(",
            "kernel void mora_fused_elementwise_f32(",
        ] {
            assert!(
                MORA_METAL_MSL.contains(kernel),
                "missing Metal MSL kernel: {kernel}"
            );
        }
        let opens = MORA_METAL_MSL.matches('{').count();
        let closes = MORA_METAL_MSL.matches('}').count();
        assert_eq!(opens, closes, "unbalanced braces in Metal MSL source");
    }

    #[test]
    fn metal_custom_kernel_scalar_packing_matches_msl_buffer_layout() {
        let prog = KernelProgram {
            name: "saxpy_msl".to_string(),
            params: vec![
                KernelParam {
                    name: "x".to_string(),
                    ty: KernelParamType::BufF32,
                },
                KernelParam {
                    name: "y".to_string(),
                    ty: KernelParamType::BufF32,
                },
                KernelParam {
                    name: "alpha".to_string(),
                    ty: KernelParamType::F32,
                },
                KernelParam {
                    name: "n".to_string(),
                    ty: KernelParamType::I32,
                },
            ],
            body: Vec::new(),
        };
        let msl = emit_msl(&prog);
        assert!(msl.contains("device float* x [[buffer(0)]]"));
        assert!(msl.contains("device float* y [[buffer(1)]]"));
        assert!(msl.contains("constant _MoraKernelParams& _kp [[buffer(2)]]"));
        assert!(msl.contains("float alpha;"));
        assert!(msl.contains("int n;"));
        let words = pack_metal_custom_kernel_scalars(&[
            KernelLaunchArg::F32(2.5),
            KernelLaunchArg::I32(-7),
        ]);
        assert_eq!(words, vec![2.5f32.to_bits(), (-7i32) as u32]);
    }
}
