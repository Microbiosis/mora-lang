//! Wave 4 P2-2: Native NVIDIA CUDA Driver API (`nvcuda.dll` / `libcuda.so.1`) Hardware Backend.
//!
//! Architecture (`AGENTS.md §0`):
//! - Zero compile-time CUDA Toolkit / `nvcc` / Cargo dependency: dynamically opens the system
//!   NVIDIA driver (`nvcuda.dll` on Windows, `libcuda.so.1` / `libcuda.so` on Linux) at runtime.
//! - Uses NVIDIA's built-in driver PTX JIT (`cuModuleLoadData`) to compile Mora's native PTX ISA 7.0
//!   (`sm_50`, forward-compatible through Ampere/Ada/Hopper/Blackwell SM 12.0) `f64` GPU kernels.
//! - Implements ALL 5 [`DeviceBackend`] fused kernels directly on hardware GPU VRAM:
//!   1. `mora_matmul_f64` (2D & broadcast batched ND GEMM)
//!   2. `mora_fused_linear_act_f64` (GEMM + bias + Relu/Gelu/Silu/Tanh/Sigmoid in registers)
//!   3. `mora_rms_inv_f64` + `mora_fused_rms_linear_act_f64` (RMSNorm [* gamma] + Linear [+ bias] [+ Act])
//!   4. `mora_fused_flash_attention_f64` (online-softmax FlashAttention-2 with zero O(Sq*Sk) VRAM)
//!   5. `mora_fused_ew_f64` (single-launch bytecode-tape fused elementwise broadcast evaluator)
//! - Includes a per-device size-classed VRAM slab arena (`CudaVramArena`) recycling `CUdeviceptr`
//!   allocations across repeated DAG invocations with deterministic `Drop` cleanup.

use std::collections::BTreeMap;
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::ptr;
use std::sync::Arc;

use parking_lot::Mutex;

use crate::common::{DType, Device};
use crate::error::{MoraError, MoraResult};
use crate::runtime::compute_dag::{DeviceBackend, FusedAct, FusedEwStep};
use crate::runtime::gpu::{DynLib, GpuDeviceInfo, HardwareBackendEntry};
use crate::runtime::simd_kernels;

type CuResult = c_int;
type CuDevice = c_int;
type CuContext = *mut c_void;
type CuModule = *mut c_void;
type CuFunction = *mut c_void;
type CuStream = *mut c_void;
type CuDevicePtr = u64;

const CUDA_SUCCESS: CuResult = 0;
const CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT: c_int = 16;
const CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR: c_int = 75;
const CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR: c_int = 76;
const CU_JIT_ERROR_LOG_BUFFER: c_uint = 5;
const CU_JIT_ERROR_LOG_BUFFER_SIZE_BYTES: c_uint = 6;

type FnCuInit = unsafe extern "C" fn(c_uint) -> CuResult;
type FnCuDriverGetVersion = unsafe extern "C" fn(*mut c_int) -> CuResult;
type FnCuDeviceGetCount = unsafe extern "C" fn(*mut c_int) -> CuResult;
type FnCuDeviceGet = unsafe extern "C" fn(*mut CuDevice, c_int) -> CuResult;
type FnCuDeviceGetName = unsafe extern "C" fn(*mut c_char, c_int, CuDevice) -> CuResult;
type FnCuDeviceTotalMemV2 = unsafe extern "C" fn(*mut usize, CuDevice) -> CuResult;
type FnCuDeviceGetAttribute = unsafe extern "C" fn(*mut c_int, c_int, CuDevice) -> CuResult;
type FnCuDevicePrimaryCtxRetain = unsafe extern "C" fn(*mut CuContext, CuDevice) -> CuResult;
type FnCuDevicePrimaryCtxReleaseV2 = unsafe extern "C" fn(CuDevice) -> CuResult;
type FnCuCtxSetCurrent = unsafe extern "C" fn(CuContext) -> CuResult;
type FnCuCtxSynchronize = unsafe extern "C" fn() -> CuResult;
type FnCuMemAllocV2 = unsafe extern "C" fn(*mut CuDevicePtr, usize) -> CuResult;
type FnCuMemFreeV2 = unsafe extern "C" fn(CuDevicePtr) -> CuResult;
type FnCuMemcpyHtoDV2 = unsafe extern "C" fn(CuDevicePtr, *const c_void, usize) -> CuResult;
type FnCuMemcpyDtoHV2 = unsafe extern "C" fn(*mut c_void, CuDevicePtr, usize) -> CuResult;
type FnCuModuleLoadData = unsafe extern "C" fn(*mut CuModule, *const c_void) -> CuResult;
type FnCuModuleLoadDataEx = unsafe extern "C" fn(
    *mut CuModule,
    *const c_void,
    c_uint,
    *mut c_uint,
    *mut *mut c_void,
) -> CuResult;
type FnCuModuleUnload = unsafe extern "C" fn(CuModule) -> CuResult;
type FnCuModuleGetFunction =
    unsafe extern "C" fn(*mut CuFunction, CuModule, *const c_char) -> CuResult;
type FnCuLaunchKernel = unsafe extern "C" fn(
    CuFunction,
    c_uint,
    c_uint,
    c_uint,
    c_uint,
    c_uint,
    c_uint,
    c_uint,
    CuStream,
    *mut *mut c_void,
    *mut *mut c_void,
) -> CuResult;

struct CudaDriverApi {
    _lib: DynLib,
    cu_init: FnCuInit,
    cu_driver_get_version: FnCuDriverGetVersion,
    cu_device_get_count: FnCuDeviceGetCount,
    cu_device_get: FnCuDeviceGet,
    cu_device_get_name: FnCuDeviceGetName,
    cu_device_total_mem: FnCuDeviceTotalMemV2,
    cu_device_get_attribute: FnCuDeviceGetAttribute,
    cu_primary_ctx_retain: FnCuDevicePrimaryCtxRetain,
    cu_primary_ctx_release: FnCuDevicePrimaryCtxReleaseV2,
    cu_ctx_set_current: FnCuCtxSetCurrent,
    cu_ctx_synchronize: FnCuCtxSynchronize,
    cu_mem_alloc: FnCuMemAllocV2,
    cu_mem_free: FnCuMemFreeV2,
    cu_memcpy_htod: FnCuMemcpyHtoDV2,
    cu_memcpy_dtoh: FnCuMemcpyDtoHV2,
    cu_module_load_data: FnCuModuleLoadData,
    cu_module_load_data_ex: Option<FnCuModuleLoadDataEx>,
    cu_module_unload: FnCuModuleUnload,
    cu_module_get_function: FnCuModuleGetFunction,
    cu_launch_kernel: FnCuLaunchKernel,
}

impl CudaDriverApi {
    fn load() -> Option<Arc<Self>> {
        let candidates: &[&str] = if cfg!(windows) {
            &["nvcuda.dll"]
        } else if cfg!(target_os = "linux") {
            &["libcuda.so.1", "libcuda.so"]
        } else {
            return None;
        };
        let lib = DynLib::open_any(candidates)?;
        // SAFETY: Every symbol name matches the official NVIDIA CUDA Driver API C ABI (`cuda.h`).
        unsafe {
            let cu_init: FnCuInit = lib.get_symbol(b"cuInit\0")?;
            let cu_driver_get_version: FnCuDriverGetVersion =
                lib.get_symbol(b"cuDriverGetVersion\0")?;
            let cu_device_get_count: FnCuDeviceGetCount = lib.get_symbol(b"cuDeviceGetCount\0")?;
            let cu_device_get: FnCuDeviceGet = lib.get_symbol(b"cuDeviceGet\0")?;
            let cu_device_get_name: FnCuDeviceGetName = lib.get_symbol(b"cuDeviceGetName\0")?;
            let cu_device_total_mem: FnCuDeviceTotalMemV2 = lib
                .get_symbol(b"cuDeviceTotalMem_v2\0")
                .or_else(|| lib.get_symbol(b"cuDeviceTotalMem\0"))?;
            let cu_device_get_attribute: FnCuDeviceGetAttribute =
                lib.get_symbol(b"cuDeviceGetAttribute\0")?;
            let cu_primary_ctx_retain: FnCuDevicePrimaryCtxRetain =
                lib.get_symbol(b"cuDevicePrimaryCtxRetain\0")?;
            let cu_primary_ctx_release: FnCuDevicePrimaryCtxReleaseV2 = lib
                .get_symbol(b"cuDevicePrimaryCtxRelease_v2\0")
                .or_else(|| lib.get_symbol(b"cuDevicePrimaryCtxRelease\0"))?;
            let cu_ctx_set_current: FnCuCtxSetCurrent = lib.get_symbol(b"cuCtxSetCurrent\0")?;
            let cu_ctx_synchronize: FnCuCtxSynchronize = lib.get_symbol(b"cuCtxSynchronize\0")?;
            let cu_mem_alloc: FnCuMemAllocV2 = lib
                .get_symbol(b"cuMemAlloc_v2\0")
                .or_else(|| lib.get_symbol(b"cuMemAlloc\0"))?;
            let cu_mem_free: FnCuMemFreeV2 = lib
                .get_symbol(b"cuMemFree_v2\0")
                .or_else(|| lib.get_symbol(b"cuMemFree\0"))?;
            let cu_memcpy_htod: FnCuMemcpyHtoDV2 = lib
                .get_symbol(b"cuMemcpyHtoD_v2\0")
                .or_else(|| lib.get_symbol(b"cuMemcpyHtoD\0"))?;
            let cu_memcpy_dtoh: FnCuMemcpyDtoHV2 = lib
                .get_symbol(b"cuMemcpyDtoH_v2\0")
                .or_else(|| lib.get_symbol(b"cuMemcpyDtoH\0"))?;
            let cu_module_load_data: FnCuModuleLoadData = lib.get_symbol(b"cuModuleLoadData\0")?;
            let cu_module_load_data_ex: Option<FnCuModuleLoadDataEx> =
                lib.get_symbol(b"cuModuleLoadDataEx\0");
            let cu_module_unload: FnCuModuleUnload = lib.get_symbol(b"cuModuleUnload\0")?;
            let cu_module_get_function: FnCuModuleGetFunction =
                lib.get_symbol(b"cuModuleGetFunction\0")?;
            let cu_launch_kernel: FnCuLaunchKernel = lib.get_symbol(b"cuLaunchKernel\0")?;

            Some(Arc::new(Self {
                _lib: lib,
                cu_init,
                cu_driver_get_version,
                cu_device_get_count,
                cu_device_get,
                cu_device_get_name,
                cu_device_total_mem,
                cu_device_get_attribute,
                cu_primary_ctx_retain,
                cu_primary_ctx_release,
                cu_ctx_set_current,
                cu_ctx_synchronize,
                cu_mem_alloc,
                cu_mem_free,
                cu_memcpy_htod,
                cu_memcpy_dtoh,
                cu_module_load_data,
                cu_module_load_data_ex,
                cu_module_unload,
                cu_module_get_function,
                cu_launch_kernel,
            }))
        }
    }
}

fn cu_check(res: CuResult, op: &str) -> MoraResult<()> {
    if res == CUDA_SUCCESS {
        Ok(())
    } else {
        Err(MoraError::Runtime(format!(
            "CUDA Driver API error during {op}: CUresult({res})"
        )))
    }
}

/// Power-of-two size-classed VRAM slab arena recycling `CUdeviceptr` allocations inside a context.
struct CudaVramArena {
    free_slabs: BTreeMap<usize, Vec<CuDevicePtr>>,
}

impl CudaVramArena {
    fn new() -> Self {
        Self {
            free_slabs: BTreeMap::new(),
        }
    }

    fn bucket(bytes: usize) -> usize {
        let min = bytes.max(256);
        min.checked_next_power_of_two().unwrap_or(min)
    }

    fn alloc(&mut self, api: &CudaDriverApi, bytes: usize) -> MoraResult<(CuDevicePtr, usize)> {
        let cap = Self::bucket(bytes);
        if let Some(list) = self.free_slabs.get_mut(&cap)
            && let Some(ptr) = list.pop()
        {
            return Ok((ptr, cap));
        }
        let mut dptr: CuDevicePtr = 0;
        // SAFETY: Caller has bound the device CUDA primary context to the current thread.
        let rc = unsafe { (api.cu_mem_alloc)(&mut dptr, cap) };
        cu_check(rc, "cuMemAlloc_v2")?;
        Ok((dptr, cap))
    }

    fn release(&mut self, api: &CudaDriverApi, ptr: CuDevicePtr, cap: usize) {
        if ptr == 0 {
            return;
        }
        let bucket = self.free_slabs.entry(cap).or_default();
        if bucket.len() < 8 {
            bucket.push(ptr);
        } else {
            // SAFETY: `ptr` was allocated via `cuMemAlloc_v2` in the bound context.
            unsafe {
                let _ = (api.cu_mem_free)(ptr);
            }
        }
    }

    fn drain(&mut self, api: &CudaDriverApi) {
        for (_, ptrs) in std::mem::take(&mut self.free_slabs) {
            for ptr in ptrs {
                if ptr != 0 {
                    // SAFETY: Every cached `ptr` was allocated via `cuMemAlloc_v2` in this context.
                    unsafe {
                        let _ = (api.cu_mem_free)(ptr);
                    }
                }
            }
        }
    }
}

struct CudaKernels {
    module: CuModule,
    matmul_f64: CuFunction,
    fused_linear_act_f64: CuFunction,
    rms_inv_f64: CuFunction,
    fused_rms_linear_act_f64: CuFunction,
    fused_flash_attention_f64: CuFunction,
    fused_ew_f64: CuFunction,
}

struct CudaContextState {
    ctx: CuContext,
    kernels: CudaKernels,
    arena: CudaVramArena,
}

/// Live NVIDIA CUDA hardware [`DeviceBackend`] executing native PTX ISA 7.0 `f64` kernels on VRAM.
pub(crate) struct CudaDeviceBackend {
    device: Device,
    cu_dev: CuDevice,
    api: Arc<CudaDriverApi>,
    info: GpuDeviceInfo,
    /// Serializes context binding + arena/kernel launch safely across concurrent Mora `spawn` tasks.
    state: Mutex<CudaContextState>,
}

// SAFETY: `CudaDeviceBackend` guards its `CUcontext`, `CUmodule`, `CUfunction`s, and `CudaVramArena`
// inside a `Mutex`, and binds `cuCtxSetCurrent` on every locked operation before calling CUDA APIs.
unsafe impl Send for CudaDeviceBackend {}
// SAFETY: All CUDA operations occur strictly under `self.state.lock()` with `cuCtxSetCurrent`.
unsafe impl Sync for CudaDeviceBackend {}

impl Drop for CudaDeviceBackend {
    fn drop(&mut self) {
        let mut st = self.state.lock();
        // SAFETY: Context is valid until `cuDevicePrimaryCtxRelease_v2` below.
        unsafe {
            let _ = (self.api.cu_ctx_set_current)(st.ctx);
            st.arena.drain(&self.api);
            if !st.kernels.module.is_null() {
                let _ = (self.api.cu_module_unload)(st.kernels.module);
                st.kernels.module = ptr::null_mut();
            }
            let _ = (self.api.cu_primary_ctx_release)(self.cu_dev);
        }
    }
}

/// Native PTX ISA 7.0 (`sm_50`) double-precision GPU compute kernels for Mora `TensorComputeDag`.
/// Forward-compatible across Maxwell / Pascal / Turing / Ampere / Ada / Hopper / Blackwell GPUs
/// via NVIDIA's driver-integrated PTX JIT compiler (zero `nvcc` / CUDA Toolkit required).
const MORA_CUDA_PTX: &str = r#"
.version 7.0
.target sm_50
.address_size 64

// Exact double-precision exp(x) via range reduction x = k*ln(2) + r (|r| <= 0.3466)
// and 11-term Horner polynomial + IEEE-754 exponent scaling (< 1e-14 relative error).
.func (.reg .f64 res) mora_exp_f64(.reg .f64 x)
{
    .reg .pred %p0;
    .reg .f64 %fd<10>;
    .reg .s64 %rd<4>;

    // Underflow clamp: x <= -700.0 -> 0.0
    setp.le.f64 %p0, x, 0dc085e00000000000;
    @%p0 bra EXP_ZERO;

    // Overflow clamp: x = min(x, 700.0)
    min.f64 %fd0, x, 0d4085e00000000000;

    // k = rint(x * log2(e))
    mul.rn.f64 %fd1, %fd0, 0d3ff71547652b82fe;
    cvt.rni.s64.f64 %rd0, %fd1;
    cvt.rn.f64.s64 %fd2, %rd0;

    // r = x - k * ln(2)
    mul.rn.f64 %fd3, %fd2, 0d3fe62e42fefa39ef;
    sub.rn.f64 %fd4, %fd0, %fd3;

    // 11-term Horner Taylor evaluation of exp(r) (bit-exact IEEE-754 f64 constants)
    mov.f64 %fd5, 0d3e5ae64567f544e4; // 1/11!
    fma.rn.f64 %fd5, %fd5, %fd4, 0d3e927e4fb7789f5c; // 1/10!
    fma.rn.f64 %fd5, %fd5, %fd4, 0d3ec71de3a556c734; // 1/9!
    fma.rn.f64 %fd5, %fd5, %fd4, 0d3efa01a01a01a01a; // 1/8!
    fma.rn.f64 %fd5, %fd5, %fd4, 0d3f2a01a01a01a01a; // 1/7!
    fma.rn.f64 %fd5, %fd5, %fd4, 0d3f56c16c16c16c17; // 1/6!
    fma.rn.f64 %fd5, %fd5, %fd4, 0d3f81111111111111; // 1/5!
    fma.rn.f64 %fd5, %fd5, %fd4, 0d3fa5555555555555; // 1/4!
    fma.rn.f64 %fd5, %fd5, %fd4, 0d3fc5555555555555; // 1/3!
    fma.rn.f64 %fd5, %fd5, %fd4, 0d3fe0000000000000; // 1/2!
    fma.rn.f64 %fd5, %fd5, %fd4, 0d3ff0000000000000; // 1/1!
    fma.rn.f64 %fd5, %fd5, %fd4, 0d3ff0000000000000; // + 1.0

    // pow2k = bitcast((k + 1023) << 52)
    add.s64 %rd1, %rd0, 1023;
    shl.b64 %rd2, %rd1, 52;
    mov.b64 %fd6, %rd2;
    mul.rn.f64 res, %fd5, %fd6;
    ret;

EXP_ZERO:
    mov.f64 res, 0d0000000000000000;
    ret;
}

// Numerically stable f64 tanh(x)
.func (.reg .f64 res) mora_tanh_f64(.reg .f64 x)
{
    .reg .pred %p0;
    .reg .f64 %fd<6>;

    setp.ge.f64 %p0, x, 0d0000000000000000;
    @%p0 bra TANH_POS;

    // x < 0: e = exp(2*x); (e - 1) / (e + 1)
    mul.rn.f64 %fd0, x, 0d4000000000000000;
    call (%fd1), mora_exp_f64, (%fd0);
    sub.rn.f64 %fd2, %fd1, 0d3ff0000000000000;
    add.rn.f64 %fd3, %fd1, 0d3ff0000000000000;
    div.rn.f64 res, %fd2, %fd3;
    ret;

TANH_POS:
    // x >= 0: e = exp(-2*x); (1 - e) / (1 + e)
    mul.rn.f64 %fd0, x, 0dc000000000000000;
    call (%fd1), mora_exp_f64, (%fd0);
    sub.rn.f64 %fd2, 0d3ff0000000000000, %fd1;
    add.rn.f64 %fd3, 0d3ff0000000000000, %fd1;
    div.rn.f64 res, %fd2, %fd3;
    ret;
}

// Numerically stable f64 sigmoid(x)
.func (.reg .f64 res) mora_sigmoid_f64(.reg .f64 x)
{
    .reg .pred %p0;
    .reg .f64 %fd<4>;

    setp.ge.f64 %p0, x, 0d0000000000000000;
    @%p0 bra SIG_POS;

    call (%fd0), mora_exp_f64, (x);
    add.rn.f64 %fd1, 0d3ff0000000000000, %fd0;
    div.rn.f64 res, %fd0, %fd1;
    ret;

SIG_POS:
    neg.f64 %fd0, x;
    call (%fd1), mora_exp_f64, (%fd0);
    add.rn.f64 %fd2, 0d3ff0000000000000, %fd1;
    div.rn.f64 res, 0d3ff0000000000000, %fd2;
    ret;
}

// Exact f64 ln(x) via f32 lg2 seed + 2 Newton-Raphson refinement steps
.func (.reg .f64 res) mora_ln_f64(.reg .f64 x)
{
    .reg .f32 %f0, %f1;
    .reg .f64 %fd<6>;

    cvt.rn.f32.f64 %f0, x;
    lg2.approx.f32 %f1, %f0;
    cvt.f64.f32 %fd0, %f1;
    mul.rn.f64 %fd0, %fd0, 0d3fe62e42fefa39ef; // * ln(2)

    // Newton step 1: y = y + x * exp(-y) - 1
    neg.f64 %fd1, %fd0;
    call (%fd2), mora_exp_f64, (%fd1);
    fma.rn.f64 %fd3, x, %fd2, 0dbff0000000000000;
    add.rn.f64 %fd0, %fd0, %fd3;

    // Newton step 2
    neg.f64 %fd1, %fd0;
    call (%fd2), mora_exp_f64, (%fd1);
    fma.rn.f64 %fd3, x, %fd2, 0dbff0000000000000;
    add.rn.f64 res, %fd0, %fd3;
    ret;
}

// Apply activation code: 0=None, 1=Relu, 2=Gelu, 3=Silu, 4=Tanh, 5=Sigmoid
.func (.reg .f64 res) mora_apply_act(.reg .f64 x, .reg .u32 act)
{
    .reg .pred %p0;
    .reg .f64 %fd<8>;

    setp.eq.u32 %p0, act, 0;
    @%p0 bra ACT_NONE;
    setp.eq.u32 %p0, act, 1;
    @%p0 bra ACT_RELU;
    setp.eq.u32 %p0, act, 2;
    @%p0 bra ACT_GELU;
    setp.eq.u32 %p0, act, 3;
    @%p0 bra ACT_SILU;
    setp.eq.u32 %p0, act, 4;
    @%p0 bra ACT_TANH;
    setp.eq.u32 %p0, act, 5;
    @%p0 bra ACT_SIGMOID;

ACT_NONE:
    mov.f64 res, x;
    ret;

ACT_RELU:
    max.f64 res, x, 0d0000000000000000;
    ret;

ACT_GELU:
    // 0.5 * x * (1 + tanh(sqrt(2/pi) * (x + 0.044715 * x^3)))
    mul.rn.f64 %fd0, x, x;
    mul.rn.f64 %fd1, %fd0, x;
    fma.rn.f64 %fd2, %fd1, 0d3fa6e4e26d4801f7, x;
    mul.rn.f64 %fd3, %fd2, 0d3fe9884533d43651;
    call (%fd4), mora_tanh_f64, (%fd3);
    add.rn.f64 %fd5, 0d3ff0000000000000, %fd4;
    mul.rn.f64 %fd6, x, 0d3fe0000000000000;
    mul.rn.f64 res, %fd6, %fd5;
    ret;

ACT_SILU:
    call (%fd0), mora_sigmoid_f64, (x);
    mul.rn.f64 res, x, %fd0;
    ret;

ACT_TANH:
    call (res), mora_tanh_f64, (x);
    ret;

ACT_SIGMOID:
    call (res), mora_sigmoid_f64, (x);
    ret;
}

// Kernel 1: Batched/2D double-precision MatMul using precomputed broadcast batch offsets.
// Grid: (ceil(N/16), ceil(M/16), batches), Block: (16, 16, 1)
.visible .entry mora_matmul_f64(
    .param .u64 p_out,
    .param .u64 p_a,
    .param .u64 p_b,
    .param .u64 p_a_bases,
    .param .u64 p_b_bases,
    .param .u32 batches,
    .param .u32 m,
    .param .u32 n,
    .param .u32 k
)
{
    .reg .pred %p<4>;
    .reg .u32 %r<14>;
    .reg .u64 %rd<16>;
    .reg .f64 %fd<4>;

    ld.param.u64 %rd0, [p_out];
    ld.param.u64 %rd1, [p_a];
    ld.param.u64 %rd2, [p_b];
    ld.param.u64 %rd3, [p_a_bases];
    ld.param.u64 %rd4, [p_b_bases];
    ld.param.u32 %r0, [batches];
    ld.param.u32 %r1, [m];
    ld.param.u32 %r2, [n];
    ld.param.u32 %r3, [k];

    mov.u32 %r4, %ctaid.x;
    mov.u32 %r5, %ntid.x;
    mov.u32 %r6, %tid.x;
    mad.lo.u32 %r7, %r4, %r5, %r6; // col

    mov.u32 %r4, %ctaid.y;
    mov.u32 %r5, %ntid.y;
    mov.u32 %r6, %tid.y;
    mad.lo.u32 %r8, %r4, %r5, %r6; // row

    mov.u32 %r9, %ctaid.z; // batch

    setp.lt.u32 %p0, %r8, %r1;
    setp.lt.u32 %p1, %r7, %r2;
    setp.lt.u32 %p2, %r9, %r0;
    and.pred %p3, %p0, %p1;
    and.pred %p3, %p3, %p2;
    @!%p3 bra MM_EXIT;

    // Load a_base = a_bases[batch], b_base = b_bases[batch]
    cvt.u64.u32 %rd5, %r9;
    shl.b64 %rd6, %rd5, 3;
    add.u64 %rd7, %rd3, %rd6;
    ld.global.u64 %rd8, [%rd7]; // a_base (in elements)
    add.u64 %rd7, %rd4, %rd6;
    ld.global.u64 %rd9, [%rd7]; // b_base (in elements)

    // a_row_base = a_base + row * k
    mul.wide.u32 %rd10, %r8, %r3;
    add.u64 %rd8, %rd8, %rd10;

    mov.f64 %fd0, 0d0000000000000000;
    mov.u32 %r10, 0; // t = 0

MM_LOOP:
    setp.ge.u32 %p0, %r10, %r3;
    @%p0 bra MM_DONE;

    // A[a_row_base + t]
    cvt.u64.u32 %rd11, %r10;
    add.u64 %rd12, %rd8, %rd11;
    shl.b64 %rd12, %rd12, 3;
    add.u64 %rd12, %rd1, %rd12;
    ld.global.f64 %fd1, [%rd12];

    // B[b_base + t * n + col]
    mul.wide.u32 %rd13, %r10, %r2;
    add.u64 %rd13, %rd13, %rd9;
    cvt.u64.u32 %rd14, %r7;
    add.u64 %rd13, %rd13, %rd14;
    shl.b64 %rd13, %rd13, 3;
    add.u64 %rd13, %rd2, %rd13;
    ld.global.f64 %fd2, [%rd13];

    fma.rn.f64 %fd0, %fd1, %fd2, %fd0;

    add.u32 %r10, %r10, 1;
    bra MM_LOOP;

MM_DONE:
    // out_idx = batch * (m * n) + row * n + col
    mul.wide.u32 %rd11, %r1, %r2;
    mul.lo.u64 %rd12, %rd5, %rd11;
    mul.wide.u32 %rd13, %r8, %r2;
    add.u64 %rd12, %rd12, %rd13;
    cvt.u64.u32 %rd14, %r7;
    add.u64 %rd12, %rd12, %rd14;
    shl.b64 %rd12, %rd12, 3;
    add.u64 %rd12, %rd0, %rd12;
    st.global.f64 [%rd12], %fd0;

MM_EXIT:
    ret;
}

// Kernel 2: Fused Linear + optional Bias + Activation in GPU registers.
.visible .entry mora_fused_linear_act_f64(
    .param .u64 p_out,
    .param .u64 p_x,
    .param .u64 p_w,
    .param .u64 p_bias,
    .param .u32 has_bias,
    .param .u32 bias_len,
    .param .u32 m_total,
    .param .u32 k,
    .param .u32 n,
    .param .u32 act_code
)
{
    .reg .pred %p<3>;
    .reg .u32 %r<13>;
    .reg .u64 %rd<12>;
    .reg .f64 %fd<5>;

    ld.param.u64 %rd0, [p_out];
    ld.param.u64 %rd1, [p_x];
    ld.param.u64 %rd2, [p_w];
    ld.param.u64 %rd3, [p_bias];
    ld.param.u32 %r0, [has_bias];
    ld.param.u32 %r1, [bias_len];
    ld.param.u32 %r2, [m_total];
    ld.param.u32 %r3, [k];
    ld.param.u32 %r4, [n];
    ld.param.u32 %r5, [act_code];

    mov.u32 %r6, %ctaid.x;
    mov.u32 %r7, %ntid.x;
    mov.u32 %r8, %tid.x;
    mad.lo.u32 %r9, %r6, %r7, %r8; // col

    mov.u32 %r6, %ctaid.y;
    mov.u32 %r7, %ntid.y;
    mov.u32 %r8, %tid.y;
    mad.lo.u32 %r10, %r6, %r7, %r8; // row

    setp.lt.u32 %p0, %r10, %r2;
    setp.lt.u32 %p1, %r9, %r4;
    and.pred %p2, %p0, %p1;
    @!%p2 bra FLA_EXIT;

    mul.wide.u32 %rd4, %r10, %r3; // row * k
    mov.f64 %fd0, 0d0000000000000000;
    mov.u32 %r11, 0;

FLA_LOOP:
    setp.ge.u32 %p0, %r11, %r3;
    @%p0 bra FLA_AFTER_DOT;

    cvt.u64.u32 %rd5, %r11;
    add.u64 %rd6, %rd4, %rd5;
    shl.b64 %rd6, %rd6, 3;
    add.u64 %rd6, %rd1, %rd6;
    ld.global.f64 %fd1, [%rd6];

    mul.wide.u32 %rd7, %r11, %r4;
    cvt.u64.u32 %rd8, %r9;
    add.u64 %rd7, %rd7, %rd8;
    shl.b64 %rd7, %rd7, 3;
    add.u64 %rd7, %rd2, %rd7;
    ld.global.f64 %fd2, [%rd7];

    fma.rn.f64 %fd0, %fd1, %fd2, %fd0;
    add.u32 %r11, %r11, 1;
    bra FLA_LOOP;

FLA_AFTER_DOT:
    setp.eq.u32 %p0, %r0, 0;
    @%p0 bra FLA_ACT;
    // bias_idx = col % bias_len (supports both scalar [1] and per-col [N] bias)
    rem.u32 %r12, %r9, %r1;
    cvt.u64.u32 %rd5, %r12;
    shl.b64 %rd5, %rd5, 3;
    add.u64 %rd5, %rd3, %rd5;
    ld.global.f64 %fd3, [%rd5];
    add.rn.f64 %fd0, %fd0, %fd3;

FLA_ACT:
    call (%fd4), mora_apply_act, (%fd0, %r5);

    mul.wide.u32 %rd9, %r10, %r4;
    cvt.u64.u32 %rd10, %r9;
    add.u64 %rd9, %rd9, %rd10;
    shl.b64 %rd9, %rd9, 3;
    add.u64 %rd9, %rd0, %rd9;
    st.global.f64 [%rd9], %fd4;

FLA_EXIT:
    ret;
}

// Kernel 3a: Compute inv_rms[row] = 1.0 / sqrt(mean_t(x[row, t]^2) + eps)
.visible .entry mora_rms_inv_f64(
    .param .u64 p_inv_rms,
    .param .u64 p_x,
    .param .u32 m_total,
    .param .u32 k,
    .param .f64 eps
)
{
    .reg .pred %p0;
    .reg .u32 %r<7>;
    .reg .u64 %rd<6>;
    .reg .f64 %fd<6>;

    ld.param.u64 %rd0, [p_inv_rms];
    ld.param.u64 %rd1, [p_x];
    ld.param.u32 %r0, [m_total];
    ld.param.u32 %r1, [k];
    ld.param.f64 %fd0, [eps];

    mov.u32 %r2, %ctaid.x;
    mov.u32 %r3, %ntid.x;
    mov.u32 %r4, %tid.x;
    mad.lo.u32 %r5, %r2, %r3, %r4; // row
    setp.ge.u32 %p0, %r5, %r0;
    @%p0 bra RMS_EXIT;

    mul.wide.u32 %rd2, %r5, %r1; // row * k
    mov.f64 %fd1, 0d0000000000000000;
    mov.u32 %r6, 0;

RMS_LOOP:
    setp.ge.u32 %p0, %r6, %r1;
    @%p0 bra RMS_DONE;
    cvt.u64.u32 %rd3, %r6;
    add.u64 %rd4, %rd2, %rd3;
    shl.b64 %rd4, %rd4, 3;
    add.u64 %rd4, %rd1, %rd4;
    ld.global.f64 %fd2, [%rd4];
    fma.rn.f64 %fd1, %fd2, %fd2, %fd1;
    add.u32 %r6, %r6, 1;
    bra RMS_LOOP;

RMS_DONE:
    cvt.rn.f64.u32 %fd3, %r1;
    div.rn.f64 %fd4, %fd1, %fd3;
    add.rn.f64 %fd4, %fd4, %fd0;
    sqrt.rn.f64 %fd4, %fd4;
    div.rn.f64 %fd5, 0d3ff0000000000000, %fd4;

    cvt.u64.u32 %rd5, %r5;
    shl.b64 %rd5, %rd5, 3;
    add.u64 %rd5, %rd0, %rd5;
    st.global.f64 [%rd5], %fd5;

RMS_EXIT:
    ret;
}

// Kernel 3b: Fused RMSNorm(x) [* gamma] @ W [+ bias] [+ Act] using precomputed inv_rms[row]
.visible .entry mora_fused_rms_linear_act_f64(
    .param .u64 p_out,
    .param .u64 p_x,
    .param .u64 p_inv_rms,
    .param .u64 p_gamma,
    .param .u32 has_gamma,
    .param .u32 gamma_len,
    .param .u64 p_w,
    .param .u64 p_bias,
    .param .u32 has_bias,
    .param .u32 bias_len,
    .param .u32 m_total,
    .param .u32 k,
    .param .u32 n,
    .param .u32 act_code
)
{
    .reg .pred %p<3>;
    .reg .u32 %r<16>;
    .reg .u64 %rd<14>;
    .reg .f64 %fd<7>;

    ld.param.u64 %rd0, [p_out];
    ld.param.u64 %rd1, [p_x];
    ld.param.u64 %rd2, [p_inv_rms];
    ld.param.u64 %rd3, [p_gamma];
    ld.param.u32 %r0, [has_gamma];
    ld.param.u32 %r1, [gamma_len];
    ld.param.u64 %rd4, [p_w];
    ld.param.u64 %rd5, [p_bias];
    ld.param.u32 %r2, [has_bias];
    ld.param.u32 %r3, [bias_len];
    ld.param.u32 %r4, [m_total];
    ld.param.u32 %r5, [k];
    ld.param.u32 %r6, [n];
    ld.param.u32 %r7, [act_code];

    mov.u32 %r8, %ctaid.x;
    mov.u32 %r9, %ntid.x;
    mov.u32 %r10, %tid.x;
    mad.lo.u32 %r11, %r8, %r9, %r10; // col

    mov.u32 %r8, %ctaid.y;
    mov.u32 %r9, %ntid.y;
    mov.u32 %r10, %tid.y;
    mad.lo.u32 %r12, %r8, %r9, %r10; // row

    setp.lt.u32 %p0, %r12, %r4;
    setp.lt.u32 %p1, %r11, %r6;
    and.pred %p2, %p0, %p1;
    @!%p2 bra FRL_EXIT;

    // inv_rms = p_inv_rms[row]
    cvt.u64.u32 %rd6, %r12;
    shl.b64 %rd6, %rd6, 3;
    add.u64 %rd6, %rd2, %rd6;
    ld.global.f64 %fd0, [%rd6];

    mul.wide.u32 %rd7, %r12, %r5; // row * k
    mov.f64 %fd1, 0d0000000000000000;
    mov.u32 %r13, 0;

FRL_LOOP:
    setp.ge.u32 %p0, %r13, %r5;
    @%p0 bra FRL_AFTER_DOT;

    cvt.u64.u32 %rd8, %r13;
    add.u64 %rd9, %rd7, %rd8;
    shl.b64 %rd9, %rd9, 3;
    add.u64 %rd9, %rd1, %rd9;
    ld.global.f64 %fd2, [%rd9];
    mul.rn.f64 %fd2, %fd2, %fd0; // x_norm

    setp.eq.u32 %p0, %r0, 0;
    @%p0 bra FRL_NO_GAMMA;
    rem.u32 %r14, %r13, %r1;
    cvt.u64.u32 %rd10, %r14;
    shl.b64 %rd10, %rd10, 3;
    add.u64 %rd10, %rd3, %rd10;
    ld.global.f64 %fd3, [%rd10];
    mul.rn.f64 %fd2, %fd2, %fd3;

FRL_NO_GAMMA:
    mul.wide.u32 %rd11, %r13, %r6;
    cvt.u64.u32 %rd12, %r11;
    add.u64 %rd11, %rd11, %rd12;
    shl.b64 %rd11, %rd11, 3;
    add.u64 %rd11, %rd4, %rd11;
    ld.global.f64 %fd4, [%rd11];

    fma.rn.f64 %fd1, %fd2, %fd4, %fd1;
    add.u32 %r13, %r13, 1;
    bra FRL_LOOP;

FRL_AFTER_DOT:
    setp.eq.u32 %p0, %r2, 0;
    @%p0 bra FRL_ACT;
    rem.u32 %r15, %r11, %r3;
    cvt.u64.u32 %rd8, %r15;
    shl.b64 %rd8, %rd8, 3;
    add.u64 %rd8, %rd5, %rd8;
    ld.global.f64 %fd5, [%rd8];
    add.rn.f64 %fd1, %fd1, %fd5;

FRL_ACT:
    call (%fd6), mora_apply_act, (%fd1, %r7);

    mul.wide.u32 %rd12, %r12, %r6;
    cvt.u64.u32 %rd13, %r11;
    add.u64 %rd12, %rd12, %rd13;
    shl.b64 %rd12, %rd12, 3;
    add.u64 %rd12, %rd0, %rd12;
    st.global.f64 [%rd12], %fd6;

FRL_EXIT:
    ret;
}

// Kernel 4: Online-Softmax FlashAttention-2 in GPU registers (zero O(Sq*Sk) VRAM materialization).
// Grid: (ceil(d_v/16), ceil(s_q/16), batches), Block: (16, 16, 1)
.visible .entry mora_fused_flash_attention_f64(
    .param .u64 p_out,
    .param .u64 p_q,
    .param .u64 p_k,
    .param .u64 p_v,
    .param .u64 p_q_bases,
    .param .u64 p_k_bases,
    .param .u64 p_v_bases,
    .param .u64 p_mask,
    .param .u64 p_mask_bases,
    .param .u32 has_mask,
    .param .u32 mask_sq_stride,
    .param .u32 mask_sk_stride,
    .param .u32 batches,
    .param .u32 s_q,
    .param .u32 s_k,
    .param .u32 d_k,
    .param .u32 d_v,
    .param .u32 k_transposed,
    .param .f64 scale
)
{
    .reg .pred %p<5>;
    .reg .u32 %r<20>;
    .reg .u64 %rd<22>;
    .reg .f64 %fd<14>;

    ld.param.u64 %rd0, [p_out];
    ld.param.u64 %rd1, [p_q];
    ld.param.u64 %rd2, [p_k];
    ld.param.u64 %rd3, [p_v];
    ld.param.u64 %rd4, [p_q_bases];
    ld.param.u64 %rd5, [p_k_bases];
    ld.param.u64 %rd6, [p_v_bases];
    ld.param.u64 %rd7, [p_mask];
    ld.param.u64 %rd8, [p_mask_bases];
    ld.param.u32 %r0, [has_mask];
    ld.param.u32 %r1, [mask_sq_stride];
    ld.param.u32 %r2, [mask_sk_stride];
    ld.param.u32 %r3, [batches];
    ld.param.u32 %r4, [s_q];
    ld.param.u32 %r5, [s_k];
    ld.param.u32 %r6, [d_k];
    ld.param.u32 %r7, [d_v];
    ld.param.u32 %r8, [k_transposed];
    ld.param.f64 %fd0, [scale];

    mov.u32 %r9, %ctaid.x;
    mov.u32 %r10, %ntid.x;
    mov.u32 %r11, %tid.x;
    mad.lo.u32 %r12, %r9, %r10, %r11; // di

    mov.u32 %r9, %ctaid.y;
    mov.u32 %r10, %ntid.y;
    mov.u32 %r11, %tid.y;
    mad.lo.u32 %r13, %r9, %r10, %r11; // qi

    mov.u32 %r14, %ctaid.z; // batch

    setp.lt.u32 %p0, %r13, %r4;
    setp.lt.u32 %p1, %r12, %r7;
    setp.lt.u32 %p2, %r14, %r3;
    and.pred %p3, %p0, %p1;
    and.pred %p3, %p3, %p2;
    @!%p3 bra FA_EXIT;

    // Load per-batch base offsets
    cvt.u64.u32 %rd9, %r14;
    shl.b64 %rd10, %rd9, 3;
    add.u64 %rd11, %rd4, %rd10;
    ld.global.u64 %rd12, [%rd11]; // q_base
    add.u64 %rd11, %rd5, %rd10;
    ld.global.u64 %rd13, [%rd11]; // k_base
    add.u64 %rd11, %rd6, %rd10;
    ld.global.u64 %rd14, [%rd11]; // v_base
    mov.u64 %rd15, 0;
    setp.eq.u32 %p0, %r0, 0;
    @%p0 bra FA_SKIP_MBASE;
    add.u64 %rd11, %rd8, %rd10;
    ld.global.u64 %rd15, [%rd11]; // mask_base
FA_SKIP_MBASE:

    // q_row_base = q_base + qi * d_k
    mul.wide.u32 %rd16, %r13, %r6;
    add.u64 %rd12, %rd12, %rd16;

    // Online softmax state: m_i = -inf, l_i = 0.0, acc = 0.0
    mov.f64 %fd1, 0dfff0000000000000; // m_i = -inf
    mov.f64 %fd2, 0d0000000000000000; // l_i = 0
    mov.f64 %fd3, 0d0000000000000000; // acc = 0

    mov.u32 %r15, 0; // kj = 0
FA_K_LOOP:
    setp.ge.u32 %p0, %r15, %r5;
    @%p0 bra FA_DONE;

    // Compute dot(Q[qi, :], K[kj, :])
    mov.f64 %fd4, 0d0000000000000000;
    mov.u32 %r16, 0; // t = 0
FA_D_LOOP:
    setp.ge.u32 %p0, %r16, %r6;
    @%p0 bra FA_SCORE_DONE;

    cvt.u64.u32 %rd17, %r16;
    add.u64 %rd18, %rd12, %rd17;
    shl.b64 %rd18, %rd18, 3;
    add.u64 %rd18, %rd1, %rd18;
    ld.global.f64 %fd5, [%rd18];

    setp.ne.u32 %p1, %r8, 0;
    @%p1 bra FA_K_TRANS;
    // K[k_base + kj * d_k + t]
    mul.wide.u32 %rd19, %r15, %r6;
    add.u64 %rd19, %rd19, %rd13;
    add.u64 %rd19, %rd19, %rd17;
    bra FA_K_LOAD;
FA_K_TRANS:
    // K[k_base + t * s_k + kj]
    mul.wide.u32 %rd19, %r16, %r5;
    add.u64 %rd19, %rd19, %rd13;
    cvt.u64.u32 %rd20, %r15;
    add.u64 %rd19, %rd19, %rd20;
FA_K_LOAD:
    shl.b64 %rd19, %rd19, 3;
    add.u64 %rd19, %rd2, %rd19;
    ld.global.f64 %fd6, [%rd19];

    fma.rn.f64 %fd4, %fd5, %fd6, %fd4;
    add.u32 %r16, %r16, 1;
    bra FA_D_LOOP;

FA_SCORE_DONE:
    mul.rn.f64 %fd4, %fd4, %fd0; // score *= scale
    setp.eq.u32 %p0, %r0, 0;
    @%p0 bra FA_NO_MASK;
    // mask_idx = mask_base + qi * mask_sq_stride + kj * mask_sk_stride
    mul.wide.u32 %rd17, %r13, %r1;
    mul.wide.u32 %rd18, %r15, %r2;
    add.u64 %rd17, %rd17, %rd18;
    add.u64 %rd17, %rd17, %rd15;
    shl.b64 %rd17, %rd17, 3;
    add.u64 %rd17, %rd7, %rd17;
    ld.global.f64 %fd7, [%rd17];
    add.rn.f64 %fd4, %fd4, %fd7;
FA_NO_MASK:
    // Skip -inf masked keys cleanly
    setp.le.f64 %p0, %fd4, 0dc7efffffe0000000;
    @%p0 bra FA_NEXT_K;

    // m_new = max(m_i, score)
    max.f64 %fd8, %fd1, %fd4;
    // alpha = (l_i == 0.0) ? 0.0 : exp(m_i - m_new)
    setp.eq.f64 %p0, %fd2, 0d0000000000000000;
    @%p0 bra FA_ALPHA_ZERO;
    sub.rn.f64 %fd9, %fd1, %fd8;
    call (%fd10), mora_exp_f64, (%fd9);
    bra FA_ALPHA_DONE;
FA_ALPHA_ZERO:
    mov.f64 %fd10, 0d0000000000000000;
FA_ALPHA_DONE:
    // p = exp(score - m_new)
    sub.rn.f64 %fd11, %fd4, %fd8;
    call (%fd12), mora_exp_f64, (%fd11);

    // Load V[v_base + kj * d_v + di]
    mul.wide.u32 %rd19, %r15, %r7;
    add.u64 %rd19, %rd19, %rd14;
    cvt.u64.u32 %rd20, %r12;
    add.u64 %rd19, %rd19, %rd20;
    shl.b64 %rd19, %rd19, 3;
    add.u64 %rd19, %rd3, %rd19;
    ld.global.f64 %fd13, [%rd19];

    // l_i = l_i * alpha + p; acc = acc * alpha + p * v; m_i = m_new
    fma.rn.f64 %fd2, %fd2, %fd10, %fd12;
    mul.rn.f64 %fd3, %fd3, %fd10;
    fma.rn.f64 %fd3, %fd12, %fd13, %fd3;
    mov.f64 %fd1, %fd8;

FA_NEXT_K:
    add.u32 %r15, %r15, 1;
    bra FA_K_LOOP;

FA_DONE:
    setp.gt.f64 %p0, %fd2, 0d0000000000000000;
    @!%p0 bra FA_OUT_ZERO;
    div.rn.f64 %fd3, %fd3, %fd2;
    bra FA_STORE;
FA_OUT_ZERO:
    mov.f64 %fd3, 0d0000000000000000;
FA_STORE:
    // out_idx = batch * (s_q * d_v) + qi * d_v + di
    mul.wide.u32 %rd17, %r4, %r7;
    mul.lo.u64 %rd18, %rd9, %rd17;
    mul.wide.u32 %rd19, %r13, %r7;
    add.u64 %rd18, %rd18, %rd19;
    cvt.u64.u32 %rd20, %r12;
    add.u64 %rd18, %rd18, %rd20;
    shl.b64 %rd18, %rd18, 3;
    add.u64 %rd18, %rd0, %rd18;
    st.global.f64 [%rd18], %fd3;

FA_EXIT:
    ret;
}

// Kernel 5: Single-launch bytecode-tape fused elementwise broadcast evaluator (<= 64 live slots).
// Opcodes:
//  0=Add, 1=Sub, 2=Mul, 3=Div, 4=Pow,
//  5=AddS, 6=SubS(a-s), 7=SubSRev(s-a), 8=MulS, 9=DivS(a/s), 10=DivSRev(s/a),
// 11=PowS(a^s), 12=PowSRev(s^a), 13=Neg, 14=Relu, 15=Gelu, 16=Silu,
// 17=Tanh, 18=Sigmoid, 19=Exp, 20=Ln, 21=Sqrt, 22=Sin, 23=Cos, 24=Abs
.visible .entry mora_fused_ew_f64(
    .param .u64 p_out,
    .param .u64 p_in_ptrs,
    .param .u64 p_in_strides,
    .param .u64 p_out_shape,
    .param .u32 ndim,
    .param .u32 n_inputs,
    .param .u64 p_ops,
    .param .u64 p_arg_a,
    .param .u64 p_arg_b,
    .param .u64 p_imm,
    .param .u32 n_steps,
    .param .u32 numel
)
{
    .local .align 8 .f64 slots[64];
    .local .align 4 .u32 coords[16];
    .reg .pred %p<3>;
    .reg .u32 %r<16>;
    .reg .u64 %rd<20>;
    .reg .f32 %f0, %f1;
    .reg .f64 %fd<8>;

    mov.u64 %rd18, slots;
    mov.u64 %rd19, coords;

    ld.param.u64 %rd0, [p_out];
    ld.param.u64 %rd1, [p_in_ptrs];
    ld.param.u64 %rd2, [p_in_strides];
    ld.param.u64 %rd3, [p_out_shape];
    ld.param.u32 %r0, [ndim];
    ld.param.u32 %r1, [n_inputs];
    ld.param.u64 %rd4, [p_ops];
    ld.param.u64 %rd5, [p_arg_a];
    ld.param.u64 %rd6, [p_arg_b];
    ld.param.u64 %rd7, [p_imm];
    ld.param.u32 %r2, [n_steps];
    ld.param.u32 %r3, [numel];

    mov.u32 %r4, %ctaid.x;
    mov.u32 %r5, %ntid.x;
    mov.u32 %r6, %tid.x;
    mad.lo.u32 %r7, %r4, %r5, %r6; // tid
    setp.ge.u32 %p0, %r7, %r3;
    @%p0 bra EW_EXIT;

    // Decompose tid into multi-dim coords[0..ndim]
    mov.u32 %r8, %r7; // rem
    mov.u32 %r9, %r0; // d_plus_1 = ndim
EW_DECOMP:
    setp.eq.u32 %p0, %r9, 0;
    @%p0 bra EW_LOAD_INS;
    sub.u32 %r9, %r9, 1;
    cvt.u64.u32 %rd8, %r9;
    shl.b64 %rd9, %rd8, 2;
    add.u64 %rd10, %rd3, %rd9;
    ld.global.u32 %r10, [%rd10]; // dim_size
    rem.u32 %r11, %r8, %r10;
    div.u32 %r8, %r8, %r10;
    add.u64 %rd16, %rd19, %rd9;
    st.local.u32 [%rd16], %r11;
    bra EW_DECOMP;

EW_LOAD_INS:
    mov.u32 %r9, 0; // inp = 0
EW_IN_LOOP:
    setp.ge.u32 %p0, %r9, %r1;
    @%p0 bra EW_STEPS;

    // Compute broadcast linear offset for input r9
    mov.u64 %rd11, 0;
    mov.u32 %r10, 0; // d = 0
    mul.lo.u32 %r11, %r9, %r0; // inp * ndim
EW_STRIDE_LOOP:
    setp.ge.u32 %p0, %r10, %r0;
    @%p0 bra EW_IN_FETCH;
    cvt.u64.u32 %rd8, %r10;
    shl.b64 %rd9, %rd8, 2;
    add.u64 %rd16, %rd19, %rd9;
    ld.local.u32 %r12, [%rd16];
    add.u32 %r13, %r11, %r10;
    cvt.u64.u32 %rd12, %r13;
    shl.b64 %rd12, %rd12, 3;
    add.u64 %rd12, %rd2, %rd12;
    ld.global.u64 %rd13, [%rd12]; // stride
    cvt.u64.u32 %rd14, %r12;
    mad.lo.u64 %rd11, %rd14, %rd13, %rd11;
    add.u32 %r10, %r10, 1;
    bra EW_STRIDE_LOOP;

EW_IN_FETCH:
    cvt.u64.u32 %rd8, %r9;
    shl.b64 %rd9, %rd8, 3;
    add.u64 %rd12, %rd1, %rd9;
    ld.global.u64 %rd13, [%rd12]; // in_ptr
    shl.b64 %rd14, %rd11, 3;
    add.u64 %rd13, %rd13, %rd14;
    ld.global.f64 %fd0, [%rd13];
    add.u64 %rd16, %rd18, %rd9;
    st.local.f64 [%rd16], %fd0;
    add.u32 %r9, %r9, 1;
    bra EW_IN_LOOP;

EW_STEPS:
    mov.u32 %r9, 0; // s = 0
EW_STEP_LOOP:
    setp.ge.u32 %p0, %r9, %r2;
    @%p0 bra EW_WRITE_OUT;

    cvt.u64.u32 %rd8, %r9;
    shl.b64 %rd9, %rd8, 2;
    add.u64 %rd10, %rd4, %rd9;
    ld.global.u32 %r10, [%rd10]; // op
    add.u64 %rd10, %rd5, %rd9;
    ld.global.u32 %r11, [%rd10]; // a_slot
    add.u64 %rd10, %rd6, %rd9;
    ld.global.u32 %r12, [%rd10]; // b_slot
    shl.b64 %rd11, %rd8, 3;
    add.u64 %rd10, %rd7, %rd11;
    ld.global.f64 %fd2, [%rd10]; // imm

    cvt.u64.u32 %rd12, %r11;
    shl.b64 %rd12, %rd12, 3;
    add.u64 %rd16, %rd18, %rd12;
    ld.local.f64 %fd0, [%rd16]; // val_a

    cvt.u64.u32 %rd13, %r12;
    shl.b64 %rd13, %rd13, 3;
    add.u64 %rd16, %rd18, %rd13;
    ld.local.f64 %fd1, [%rd16]; // val_b

    setp.eq.u32 %p0, %r10, 0; @%p0 bra OP_ADD;
    setp.eq.u32 %p0, %r10, 1; @%p0 bra OP_SUB;
    setp.eq.u32 %p0, %r10, 2; @%p0 bra OP_MUL;
    setp.eq.u32 %p0, %r10, 3; @%p0 bra OP_DIV;
    setp.eq.u32 %p0, %r10, 4; @%p0 bra OP_POW;
    setp.eq.u32 %p0, %r10, 5; @%p0 bra OP_ADDS;
    setp.eq.u32 %p0, %r10, 6; @%p0 bra OP_SUBS;
    setp.eq.u32 %p0, %r10, 7; @%p0 bra OP_SUBSR;
    setp.eq.u32 %p0, %r10, 8; @%p0 bra OP_MULS;
    setp.eq.u32 %p0, %r10, 9; @%p0 bra OP_DIVS;
    setp.eq.u32 %p0, %r10, 10; @%p0 bra OP_DIVSR;
    setp.eq.u32 %p0, %r10, 11; @%p0 bra OP_POWS;
    setp.eq.u32 %p0, %r10, 12; @%p0 bra OP_POWSR;
    setp.eq.u32 %p0, %r10, 13; @%p0 bra OP_NEG;
    setp.eq.u32 %p0, %r10, 14; @%p0 bra OP_RELU;
    setp.eq.u32 %p0, %r10, 15; @%p0 bra OP_GELU;
    setp.eq.u32 %p0, %r10, 16; @%p0 bra OP_SILU;
    setp.eq.u32 %p0, %r10, 17; @%p0 bra OP_TANH;
    setp.eq.u32 %p0, %r10, 18; @%p0 bra OP_SIG;
    setp.eq.u32 %p0, %r10, 19; @%p0 bra OP_EXP;
    setp.eq.u32 %p0, %r10, 20; @%p0 bra OP_LN;
    setp.eq.u32 %p0, %r10, 21; @%p0 bra OP_SQRT;
    setp.eq.u32 %p0, %r10, 22; @%p0 bra OP_SIN;
    setp.eq.u32 %p0, %r10, 23; @%p0 bra OP_COS;
    bra OP_ABS;

OP_ADD: add.rn.f64 %fd3, %fd0, %fd1; bra EW_STORE_SLOT;
OP_SUB: sub.rn.f64 %fd3, %fd0, %fd1; bra EW_STORE_SLOT;
OP_MUL: mul.rn.f64 %fd3, %fd0, %fd1; bra EW_STORE_SLOT;
OP_DIV: div.rn.f64 %fd3, %fd0, %fd1; bra EW_STORE_SLOT;
OP_POW:
    call (%fd4), mora_ln_f64, (%fd0);
    mul.rn.f64 %fd4, %fd4, %fd1;
    call (%fd3), mora_exp_f64, (%fd4);
    bra EW_STORE_SLOT;
OP_ADDS: add.rn.f64 %fd3, %fd0, %fd2; bra EW_STORE_SLOT;
OP_SUBS: sub.rn.f64 %fd3, %fd0, %fd2; bra EW_STORE_SLOT;
OP_SUBSR: sub.rn.f64 %fd3, %fd2, %fd0; bra EW_STORE_SLOT;
OP_MULS: mul.rn.f64 %fd3, %fd0, %fd2; bra EW_STORE_SLOT;
OP_DIVS: div.rn.f64 %fd3, %fd0, %fd2; bra EW_STORE_SLOT;
OP_DIVSR: div.rn.f64 %fd3, %fd2, %fd0; bra EW_STORE_SLOT;
OP_POWS:
    setp.eq.f64 %p0, %fd2, 0d4000000000000000;
    @%p0 bra OP_POWS_SQ;
    call (%fd4), mora_ln_f64, (%fd0);
    mul.rn.f64 %fd4, %fd4, %fd2;
    call (%fd3), mora_exp_f64, (%fd4);
    bra EW_STORE_SLOT;
OP_POWS_SQ:
    mul.rn.f64 %fd3, %fd0, %fd0;
    bra EW_STORE_SLOT;
OP_POWSR:
    call (%fd4), mora_ln_f64, (%fd2);
    mul.rn.f64 %fd4, %fd4, %fd0;
    call (%fd3), mora_exp_f64, (%fd4);
    bra EW_STORE_SLOT;
OP_NEG: neg.f64 %fd3, %fd0; bra EW_STORE_SLOT;
OP_RELU: mov.u32 %r15, 1; call (%fd3), mora_apply_act, (%fd0, %r15); bra EW_STORE_SLOT;
OP_GELU: mov.u32 %r15, 2; call (%fd3), mora_apply_act, (%fd0, %r15); bra EW_STORE_SLOT;
OP_SILU: mov.u32 %r15, 3; call (%fd3), mora_apply_act, (%fd0, %r15); bra EW_STORE_SLOT;
OP_TANH: mov.u32 %r15, 4; call (%fd3), mora_apply_act, (%fd0, %r15); bra EW_STORE_SLOT;
OP_SIG: mov.u32 %r15, 5; call (%fd3), mora_apply_act, (%fd0, %r15); bra EW_STORE_SLOT;
OP_EXP: call (%fd3), mora_exp_f64, (%fd0); bra EW_STORE_SLOT;
OP_LN: call (%fd3), mora_ln_f64, (%fd0); bra EW_STORE_SLOT;
OP_SQRT: sqrt.rn.f64 %fd3, %fd0; bra EW_STORE_SLOT;
OP_SIN:
    cvt.rn.f32.f64 %f0, %fd0;
    sin.approx.f32 %f1, %f0;
    cvt.f64.f32 %fd3, %f1;
    bra EW_STORE_SLOT;
OP_COS:
    cvt.rn.f32.f64 %f0, %fd0;
    cos.approx.f32 %f1, %f0;
    cvt.f64.f32 %fd3, %f1;
    bra EW_STORE_SLOT;
OP_ABS: abs.f64 %fd3, %fd0;

EW_STORE_SLOT:
    add.u32 %r14, %r1, %r9;
    cvt.u64.u32 %rd14, %r14;
    shl.b64 %rd14, %rd14, 3;
    add.u64 %rd16, %rd18, %rd14;
    st.local.f64 [%rd16], %fd3;
    add.u32 %r9, %r9, 1;
    bra EW_STEP_LOOP;

EW_WRITE_OUT:
    add.u32 %r14, %r1, %r2;
    sub.u32 %r14, %r14, 1;
    cvt.u64.u32 %rd14, %r14;
    shl.b64 %rd14, %rd14, 3;
    add.u64 %rd16, %rd18, %rd14;
    ld.local.f64 %fd7, [%rd16];

    cvt.u64.u32 %rd15, %r7;
    shl.b64 %rd15, %rd15, 3;
    add.u64 %rd15, %rd0, %rd15;
    st.global.f64 [%rd15], %fd7;

EW_EXIT:
    ret;
}
"#;

fn load_kernel_fn(api: &CudaDriverApi, module: CuModule, name: &str) -> MoraResult<CuFunction> {
    let cname = CString::new(name).expect("valid C kernel name");
    let mut func: CuFunction = ptr::null_mut();
    // SAFETY: `module` is a live `CUmodule` in the current context and `cname` is NUL-terminated.
    let rc = unsafe { (api.cu_module_get_function)(&mut func, module, cname.as_ptr()) };
    cu_check(rc, name)?;
    Ok(func)
}

fn compile_cuda_kernels(api: &CudaDriverApi) -> MoraResult<CudaKernels> {
    let ptx_nul = CString::new(MORA_CUDA_PTX).expect("PTX contains no interior NUL");
    let mut module: CuModule = ptr::null_mut();
    let rc = if let Some(load_ex) = api.cu_module_load_data_ex {
        let mut err_buf = vec![0u8; 16384];
        let mut opts: [c_uint; 2] = [CU_JIT_ERROR_LOG_BUFFER, CU_JIT_ERROR_LOG_BUFFER_SIZE_BYTES];
        let mut vals: [*mut c_void; 2] = [
            err_buf.as_mut_ptr() as *mut c_void,
            err_buf.len() as *mut c_void,
        ];
        // SAFETY: `ptx_nul` is NUL-terminated; `opts`/`vals` match `CU_JIT_ERROR_LOG_BUFFER*` ABI.
        let rc = unsafe {
            load_ex(
                &mut module,
                ptx_nul.as_ptr() as *const c_void,
                opts.len() as c_uint,
                opts.as_mut_ptr(),
                vals.as_mut_ptr(),
            )
        };
        if rc != CUDA_SUCCESS {
            let log_end = err_buf
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(err_buf.len());
            let log = String::from_utf8_lossy(&err_buf[..log_end]);
            return Err(MoraError::Runtime(format!(
                "CUDA cuModuleLoadDataEx(MORA_CUDA_PTX) failed: CUresult({rc})\n{log}"
            )));
        }
        rc
    } else {
        // SAFETY: `ptx_nul` is a valid NUL-terminated PTX source buffer passed to `cuModuleLoadData`.
        unsafe { (api.cu_module_load_data)(&mut module, ptx_nul.as_ptr() as *const c_void) }
    };
    cu_check(rc, "cuModuleLoadData(MORA_CUDA_PTX)")?;

    let kernels = CudaKernels {
        module,
        matmul_f64: load_kernel_fn(api, module, "mora_matmul_f64")?,
        fused_linear_act_f64: load_kernel_fn(api, module, "mora_fused_linear_act_f64")?,
        rms_inv_f64: load_kernel_fn(api, module, "mora_rms_inv_f64")?,
        fused_rms_linear_act_f64: load_kernel_fn(api, module, "mora_fused_rms_linear_act_f64")?,
        fused_flash_attention_f64: load_kernel_fn(api, module, "mora_fused_flash_attention_f64")?,
        fused_ew_f64: load_kernel_fn(api, module, "mora_fused_ew_f64")?,
    };
    Ok(kernels)
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

fn quantize_slice_in_place(out: &mut [f64], dtype: DType) {
    if dtype != DType::F64 {
        for v in out.iter_mut() {
            *v = dtype.quantize_f64(*v);
        }
    }
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
                "CUDA broadcast batch shape mismatch: {a:?} vs {b:?}"
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

fn compute_broadcast_strides(in_shape: &[usize], out_shape: &[usize]) -> Vec<u64> {
    let ndim = out_shape.len();
    let mut raw_strides = vec![1u64; in_shape.len()];
    for i in (0..in_shape.len().saturating_sub(1)).rev() {
        raw_strides[i] = raw_strides[i + 1] * (in_shape[i + 1] as u64);
    }
    let mut out = vec![0u64; ndim];
    let pad = ndim.saturating_sub(in_shape.len());
    for (i, &dim) in in_shape.iter().enumerate() {
        out[pad + i] = if dim == 1 { 0 } else { raw_strides[i] };
    }
    out
}

impl CudaDeviceBackend {
    fn upload_slice<T: Copy>(
        api: &CudaDriverApi,
        arena: &mut CudaVramArena,
        slice: &[T],
    ) -> MoraResult<(CuDevicePtr, usize)> {
        let bytes = std::mem::size_of_val(slice).max(8);
        let (ptr, cap) = arena.alloc(api, bytes)?;
        if !slice.is_empty() {
            // SAFETY: `ptr` has capacity >= `size_of_val(slice)` in the active CUDA context.
            let rc = unsafe {
                (api.cu_memcpy_htod)(
                    ptr,
                    slice.as_ptr() as *const c_void,
                    std::mem::size_of_val(slice),
                )
            };
            if rc != CUDA_SUCCESS {
                arena.release(api, ptr, cap);
                return Err(MoraError::Runtime(format!(
                    "CUDA cuMemcpyHtoD_v2 failed: CUresult({rc})"
                )));
            }
        }
        Ok((ptr, cap))
    }

    fn download_slice<T: Copy>(
        api: &CudaDriverApi,
        dptr: CuDevicePtr,
        dst: &mut [T],
    ) -> MoraResult<()> {
        if dst.is_empty() {
            return Ok(());
        }
        // SAFETY: `dst` has exact byte length `size_of_val(dst)` matching the device allocation.
        let rc = unsafe {
            (api.cu_memcpy_dtoh)(
                dst.as_mut_ptr() as *mut c_void,
                dptr,
                std::mem::size_of_val(dst),
            )
        };
        cu_check(rc, "cuMemcpyDtoH_v2")
    }
}

impl DeviceBackend for CudaDeviceBackend {
    fn kind(&self) -> Device {
        self.device.clone()
    }

    fn name(&self) -> &str {
        "cuda"
    }

    fn simd_isa(&self) -> &'static str {
        "ptx-sm50-f64"
    }

    fn worker_threads(&self) -> usize {
        self.info.multiprocessors as usize
    }

    fn is_available(&self) -> bool {
        true
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
        // 1D dot / matrix-vector shapes delegate directly to CPU SIMD; 2D & batched ND run on GPU.
        if a_shape.len() < 2 || b_shape.len() < 2 {
            simd_kernels::matmul_simd_into(a, b, a_shape, b_shape, out)?;
            quantize_slice_in_place(out, dtype);
            return Ok(());
        }
        let m = a_shape[a_shape.len() - 2];
        let k = a_shape[a_shape.len() - 1];
        let k2 = b_shape[b_shape.len() - 2];
        let n = b_shape[b_shape.len() - 1];
        if k != k2 {
            return Err(MoraError::Runtime(format!(
                "CUDA MatMul inner dim mismatch: {k} != {k2}"
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

        let a_bases: Vec<u64> = (0..batches)
            .map(|bi| (map_broadcast_batch(bi, &out_batch, a_batch) * m * k) as u64)
            .collect();
        let b_bases: Vec<u64> = (0..batches)
            .map(|bi| (map_broadcast_batch(bi, &out_batch, b_batch) * k * n) as u64)
            .collect();

        let mut st = self.state.lock();
        // SAFETY: Bind this device's retained primary CUDA context to the calling thread.
        let rc_ctx = unsafe { (self.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent")?;

        let mut allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(5);
        let res = (|| -> MoraResult<()> {
            let (mut d_out, c_out) = st.arena.alloc(&self.api, out_numel * 8)?;
            allocs.push((d_out, c_out));
            let (mut d_a, c_a) = Self::upload_slice(&self.api, &mut st.arena, a)?;
            allocs.push((d_a, c_a));
            let (mut d_b, c_b) = Self::upload_slice(&self.api, &mut st.arena, b)?;
            allocs.push((d_b, c_b));
            let (mut d_ab, c_ab) = Self::upload_slice(&self.api, &mut st.arena, &a_bases)?;
            allocs.push((d_ab, c_ab));
            let (mut d_bb, c_bb) = Self::upload_slice(&self.api, &mut st.arena, &b_bases)?;
            allocs.push((d_bb, c_bb));

            let mut batches_u = batches as u32;
            let mut m_u = m as u32;
            let mut n_u = n as u32;
            let mut k_u = k as u32;
            let mut params: [*mut c_void; 9] = [
                &raw mut d_out as *mut c_void,
                &raw mut d_a as *mut c_void,
                &raw mut d_b as *mut c_void,
                &raw mut d_ab as *mut c_void,
                &raw mut d_bb as *mut c_void,
                &raw mut batches_u as *mut c_void,
                &raw mut m_u as *mut c_void,
                &raw mut n_u as *mut c_void,
                &raw mut k_u as *mut c_void,
            ];
            let gx = (n as u32).div_ceil(16).max(1);
            let gy = (m as u32).div_ceil(16).max(1);
            // SAFETY: Parameter layout matches `mora_matmul_f64` PTX entry signature exactly.
            let rc = unsafe {
                (self.api.cu_launch_kernel)(
                    st.kernels.matmul_f64,
                    gx,
                    gy,
                    batches_u,
                    16,
                    16,
                    1,
                    0,
                    ptr::null_mut(),
                    params.as_mut_ptr(),
                    ptr::null_mut(),
                )
            };
            cu_check(rc, "cuLaunchKernel(mora_matmul_f64)")?;
            // SAFETY: Synchronize default stream before D2H copy.
            let rc_sync = unsafe { (self.api.cu_ctx_synchronize)() };
            cu_check(rc_sync, "cuCtxSynchronize(mora_matmul_f64)")?;
            Self::download_slice(&self.api, d_out, out)?;
            Ok(())
        })();

        for (p, c) in allocs {
            st.arena.release(&self.api, p, c);
        }
        res?;
        quantize_slice_in_place(out, dtype);
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
        _out_shape: &[usize],
        dtype: DType,
        out: &mut Vec<f64>,
    ) -> MoraResult<()> {
        let out_numel = m_total * n;
        out.resize(out_numel, 0.0);
        if out_numel == 0 {
            return Ok(());
        }

        // Handle bias broadcast cleanly: 1D [N] or scalar [1] runs natively on GPU; unusual rank biases fall back cleanly.
        let bias_slice_opt = match bias {
            None => Some(&[][..]),
            Some((bvals, bshape)) => {
                if bvals.len() == n || bvals.len() == 1 || (bshape.len() == 1 && bshape[0] == n) {
                    Some(bvals)
                } else {
                    None
                }
            }
        };
        let Some(bias_slice) = bias_slice_opt else {
            simd_kernels::fused_linear_act_into(
                x, w, bias, act, m_total, k, n, _out_shape, dtype, out,
            );
            return Ok(());
        };

        let mut st = self.state.lock();
        // SAFETY: Bind retained primary context.
        let rc_ctx = unsafe { (self.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent")?;

        let mut allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(4);
        let res = (|| -> MoraResult<()> {
            let (mut d_out, c_out) = st.arena.alloc(&self.api, out_numel * 8)?;
            allocs.push((d_out, c_out));
            let (mut d_x, c_x) = Self::upload_slice(&self.api, &mut st.arena, x)?;
            allocs.push((d_x, c_x));
            let (mut d_w, c_w) = Self::upload_slice(&self.api, &mut st.arena, w)?;
            allocs.push((d_w, c_w));
            let (mut d_bias, has_bias_u, bias_len_u) = if !bias_slice.is_empty() {
                let (p, c) = Self::upload_slice(&self.api, &mut st.arena, bias_slice)?;
                allocs.push((p, c));
                (p, 1u32, bias_slice.len() as u32)
            } else {
                (0u64, 0u32, 1u32)
            };

            let mut hb = has_bias_u;
            let mut bl = bias_len_u;
            let mut m_u = m_total as u32;
            let mut k_u = k as u32;
            let mut n_u = n as u32;
            let mut ac = act_code(act);
            let mut params: [*mut c_void; 10] = [
                &raw mut d_out as *mut c_void,
                &raw mut d_x as *mut c_void,
                &raw mut d_w as *mut c_void,
                &raw mut d_bias as *mut c_void,
                &raw mut hb as *mut c_void,
                &raw mut bl as *mut c_void,
                &raw mut m_u as *mut c_void,
                &raw mut k_u as *mut c_void,
                &raw mut n_u as *mut c_void,
                &raw mut ac as *mut c_void,
            ];
            let gx = n_u.div_ceil(16).max(1);
            let gy = m_u.div_ceil(16).max(1);
            // SAFETY: Parameter layout matches `mora_fused_linear_act_f64` PTX signature.
            let rc = unsafe {
                (self.api.cu_launch_kernel)(
                    st.kernels.fused_linear_act_f64,
                    gx,
                    gy,
                    1,
                    16,
                    16,
                    1,
                    0,
                    ptr::null_mut(),
                    params.as_mut_ptr(),
                    ptr::null_mut(),
                )
            };
            cu_check(rc, "cuLaunchKernel(mora_fused_linear_act_f64)")?;
            // SAFETY: Synchronize default stream before D2H copy.
            let rc_sync = unsafe { (self.api.cu_ctx_synchronize)() };
            cu_check(rc_sync, "cuCtxSynchronize(mora_fused_linear_act_f64)")?;
            Self::download_slice(&self.api, d_out, out)?;
            Ok(())
        })();

        for (p, c) in allocs {
            st.arena.release(&self.api, p, c);
        }
        res?;
        quantize_slice_in_place(out, dtype);
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
        let out_numel = m_total * n;
        out.resize(out_numel, 0.0);
        if out_numel == 0 || k == 0 {
            return Ok(());
        }
        let gamma_slice_opt = match gamma {
            None => Some(&[][..]),
            Some((gvals, _)) if gvals.len() == k || gvals.len() == 1 => Some(gvals),
            Some(_) => None,
        };
        let bias_slice_opt = match bias {
            None => Some(&[][..]),
            Some((bvals, _)) if bvals.len() == n || bvals.len() == 1 => Some(bvals),
            Some(_) => None,
        };
        let (Some(gamma_slice), Some(bias_slice)) = (gamma_slice_opt, bias_slice_opt) else {
            simd_kernels::fused_rms_norm_linear_into(
                x, eps, gamma, w, bias, act, m_total, k, n, x_shape, out_shape, dtype, out,
            );
            return Ok(());
        };

        let mut st = self.state.lock();
        // SAFETY: Bind retained primary CUDA context.
        let rc_ctx = unsafe { (self.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent")?;

        let mut allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(6);
        let res = (|| -> MoraResult<()> {
            let (mut d_out, c_out) = st.arena.alloc(&self.api, out_numel * 8)?;
            allocs.push((d_out, c_out));
            let (mut d_inv, c_inv) = st.arena.alloc(&self.api, m_total * 8)?;
            allocs.push((d_inv, c_inv));
            let (mut d_x, c_x) = Self::upload_slice(&self.api, &mut st.arena, x)?;
            allocs.push((d_x, c_x));
            let (mut d_w, c_w) = Self::upload_slice(&self.api, &mut st.arena, w)?;
            allocs.push((d_w, c_w));
            let (mut d_gamma, mut hg, mut gl) = if !gamma_slice.is_empty() {
                let (p, c) = Self::upload_slice(&self.api, &mut st.arena, gamma_slice)?;
                allocs.push((p, c));
                (p, 1u32, gamma_slice.len() as u32)
            } else {
                (0u64, 0u32, 1u32)
            };
            let (mut d_bias, mut hb, mut bl) = if !bias_slice.is_empty() {
                let (p, c) = Self::upload_slice(&self.api, &mut st.arena, bias_slice)?;
                allocs.push((p, c));
                (p, 1u32, bias_slice.len() as u32)
            } else {
                (0u64, 0u32, 1u32)
            };

            let mut m_u = m_total as u32;
            let mut k_u = k as u32;
            let mut n_u = n as u32;
            let mut eps_v = eps;
            let mut ac = act_code(act);

            // Stage 1: compute inv_rms[0..m_total] on GPU (O(m_total) scratch only)
            let mut p1: [*mut c_void; 5] = [
                &raw mut d_inv as *mut c_void,
                &raw mut d_x as *mut c_void,
                &raw mut m_u as *mut c_void,
                &raw mut k_u as *mut c_void,
                &raw mut eps_v as *mut c_void,
            ];
            let rms_blocks = m_u.div_ceil(128).max(1);
            // SAFETY: Parameter layout matches `mora_rms_inv_f64` PTX signature.
            let rc1 = unsafe {
                (self.api.cu_launch_kernel)(
                    st.kernels.rms_inv_f64,
                    rms_blocks,
                    1,
                    1,
                    128,
                    1,
                    1,
                    0,
                    ptr::null_mut(),
                    p1.as_mut_ptr(),
                    ptr::null_mut(),
                )
            };
            cu_check(rc1, "cuLaunchKernel(mora_rms_inv_f64)")?;

            // Stage 2: fused normalized matmul + gamma + bias + activation on GPU
            let mut p2: [*mut c_void; 14] = [
                &raw mut d_out as *mut c_void,
                &raw mut d_x as *mut c_void,
                &raw mut d_inv as *mut c_void,
                &raw mut d_gamma as *mut c_void,
                &raw mut hg as *mut c_void,
                &raw mut gl as *mut c_void,
                &raw mut d_w as *mut c_void,
                &raw mut d_bias as *mut c_void,
                &raw mut hb as *mut c_void,
                &raw mut bl as *mut c_void,
                &raw mut m_u as *mut c_void,
                &raw mut k_u as *mut c_void,
                &raw mut n_u as *mut c_void,
                &raw mut ac as *mut c_void,
            ];
            let gx = n_u.div_ceil(16).max(1);
            let gy = m_u.div_ceil(16).max(1);
            // SAFETY: Parameter layout matches `mora_fused_rms_linear_act_f64` PTX signature.
            let rc2 = unsafe {
                (self.api.cu_launch_kernel)(
                    st.kernels.fused_rms_linear_act_f64,
                    gx,
                    gy,
                    1,
                    16,
                    16,
                    1,
                    0,
                    ptr::null_mut(),
                    p2.as_mut_ptr(),
                    ptr::null_mut(),
                )
            };
            cu_check(rc2, "cuLaunchKernel(mora_fused_rms_linear_act_f64)")?;
            // SAFETY: Synchronize default stream before D2H copy.
            let rc_sync = unsafe { (self.api.cu_ctx_synchronize)() };
            cu_check(rc_sync, "cuCtxSynchronize(mora_fused_rms_linear_act_f64)")?;
            Self::download_slice(&self.api, d_out, out)?;
            Ok(())
        })();

        for (p, c) in allocs {
            st.arena.release(&self.api, p, c);
        }
        res?;
        quantize_slice_in_place(out, dtype);
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
                "CUDA FlashAttention shape mismatch: Q={q_shape:?}, K={k_shape:?}, V={v_shape:?}"
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

        let q_bases: Vec<u64> = (0..batches)
            .map(|b| (map_broadcast_batch(b, batch_out, batch_q) * s_q * d_k) as u64)
            .collect();
        let k_bases: Vec<u64> = (0..batches)
            .map(|b| (map_broadcast_batch(b, batch_out, batch_k) * s_k * d_k) as u64)
            .collect();
        let v_bases: Vec<u64> = (0..batches)
            .map(|b| (map_broadcast_batch(b, batch_out, batch_v) * s_k * d_v) as u64)
            .collect();

        let (mask_vals, mask_bases, mut mask_sq_s, mut mask_sk_s, mut has_mask_u) =
            if let Some((mvals, mshape)) = mask {
                let mrank = mshape.len();
                let m_sk = *mshape.last().unwrap_or(&1);
                let m_sq = if mrank >= 2 { mshape[mrank - 2] } else { 1 };
                let mbatch = if mrank >= 2 {
                    &mshape[..mrank - 2]
                } else {
                    &[]
                };
                let sq_stride = if m_sq == 1 { 0 } else { m_sk as u32 };
                let sk_stride = if m_sk == 1 { 0 } else { 1u32 };
                let mbases: Vec<u64> = (0..batches)
                    .map(|b| (map_broadcast_batch(b, batch_out, mbatch) * m_sq * m_sk) as u64)
                    .collect();
                (mvals, mbases, sq_stride, sk_stride, 1u32)
            } else {
                (&[][..], Vec::new(), 0u32, 0u32, 0u32)
            };

        let mut st = self.state.lock();
        // SAFETY: Bind retained primary CUDA context.
        let rc_ctx = unsafe { (self.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent")?;

        let mut allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(9);
        let res = (|| -> MoraResult<()> {
            let (mut dev_out, c_out) = st.arena.alloc(&self.api, out_numel * 8)?;
            allocs.push((dev_out, c_out));
            let (mut dev_q, c_q) = Self::upload_slice(&self.api, &mut st.arena, q)?;
            allocs.push((dev_q, c_q));
            let (mut dev_k, c_k) = Self::upload_slice(&self.api, &mut st.arena, k)?;
            allocs.push((dev_k, c_k));
            let (mut dev_v, c_v) = Self::upload_slice(&self.api, &mut st.arena, v)?;
            allocs.push((dev_v, c_v));
            let (mut dev_qb, c_qb) = Self::upload_slice(&self.api, &mut st.arena, &q_bases)?;
            allocs.push((dev_qb, c_qb));
            let (mut dev_kb, c_kb) = Self::upload_slice(&self.api, &mut st.arena, &k_bases)?;
            allocs.push((dev_kb, c_kb));
            let (mut dev_vb, c_vb) = Self::upload_slice(&self.api, &mut st.arena, &v_bases)?;
            allocs.push((dev_vb, c_vb));
            let (mut dev_mask, mut dev_mb) = if has_mask_u != 0 {
                let (pm, cm) = Self::upload_slice(&self.api, &mut st.arena, mask_vals)?;
                allocs.push((pm, cm));
                let (pmb, cmb) = Self::upload_slice(&self.api, &mut st.arena, &mask_bases)?;
                allocs.push((pmb, cmb));
                (pm, pmb)
            } else {
                (0u64, 0u64)
            };

            let mut batches_u = batches as u32;
            let mut sq_u = s_q as u32;
            let mut sk_u = s_k as u32;
            let mut dk_u = d_k as u32;
            let mut dv_u = d_v as u32;
            let mut kt_u = u32::from(k_is_transposed);
            let mut scale_v = scale;

            let mut params: [*mut c_void; 19] = [
                &raw mut dev_out as *mut c_void,
                &raw mut dev_q as *mut c_void,
                &raw mut dev_k as *mut c_void,
                &raw mut dev_v as *mut c_void,
                &raw mut dev_qb as *mut c_void,
                &raw mut dev_kb as *mut c_void,
                &raw mut dev_vb as *mut c_void,
                &raw mut dev_mask as *mut c_void,
                &raw mut dev_mb as *mut c_void,
                &raw mut has_mask_u as *mut c_void,
                &raw mut mask_sq_s as *mut c_void,
                &raw mut mask_sk_s as *mut c_void,
                &raw mut batches_u as *mut c_void,
                &raw mut sq_u as *mut c_void,
                &raw mut sk_u as *mut c_void,
                &raw mut dk_u as *mut c_void,
                &raw mut dv_u as *mut c_void,
                &raw mut kt_u as *mut c_void,
                &raw mut scale_v as *mut c_void,
            ];
            let gx = dv_u.div_ceil(16).max(1);
            let gy = sq_u.div_ceil(16).max(1);
            // SAFETY: Parameter layout matches `mora_fused_flash_attention_f64` PTX signature.
            let rc_fa = unsafe {
                (self.api.cu_launch_kernel)(
                    st.kernels.fused_flash_attention_f64,
                    gx,
                    gy,
                    batches_u,
                    16,
                    16,
                    1,
                    0,
                    ptr::null_mut(),
                    params.as_mut_ptr(),
                    ptr::null_mut(),
                )
            };
            cu_check(rc_fa, "cuLaunchKernel(mora_fused_flash_attention_f64)")?;
            // SAFETY: Synchronize default stream before D2H copy.
            let rc_sync = unsafe { (self.api.cu_ctx_synchronize)() };
            cu_check(rc_sync, "cuCtxSynchronize(mora_fused_flash_attention_f64)")?;
            Self::download_slice(&self.api, dev_out, out)?;
            Ok(())
        })();

        for (p, c) in allocs {
            st.arena.release(&self.api, p, c);
        }
        res?;
        quantize_slice_in_place(out, dtype);
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
        if out_numel == 0 || steps.is_empty() || total_slots > 64 || out_shape.len() > 16 {
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

        let mut all_strides: Vec<u64> = Vec::with_capacity(input_shapes.len() * ndim);
        for s in input_shapes {
            all_strides.extend(compute_broadcast_strides(s, &eff_out_shape));
        }

        let mut ops = Vec::with_capacity(steps.len());
        let mut arg_a = Vec::with_capacity(steps.len());
        let mut arg_b = Vec::with_capacity(steps.len());
        let mut imm = Vec::with_capacity(steps.len());
        for step in steps {
            let (op, a, b, im) = match *step {
                FusedEwStep::Add(a, b) => (0u32, a as u32, b as u32, 0.0),
                FusedEwStep::Sub(a, b) => (1, a as u32, b as u32, 0.0),
                FusedEwStep::Mul(a, b) => (2, a as u32, b as u32, 0.0),
                FusedEwStep::Div(a, b) => (3, a as u32, b as u32, 0.0),
                FusedEwStep::Pow(a, b) => (4, a as u32, b as u32, 0.0),
                FusedEwStep::AddScalar(a, s) => (5, a as u32, 0, s),
                FusedEwStep::SubScalar(a, s, rev) => (if rev { 7 } else { 6 }, a as u32, 0, s),
                FusedEwStep::MulScalar(a, s) => (8, a as u32, 0, s),
                FusedEwStep::DivScalar(a, s, rev) => (if rev { 10 } else { 9 }, a as u32, 0, s),
                FusedEwStep::PowScalar(a, s, rev) => (if rev { 12 } else { 11 }, a as u32, 0, s),
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
            imm.push(im);
        }

        let mut st = self.state.lock();
        // SAFETY: Bind retained primary CUDA context.
        let rc_ctx = unsafe { (self.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent")?;

        let mut allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(input_flats.len() + 8);
        let res = (|| -> MoraResult<()> {
            let (mut d_out, c_out) = st.arena.alloc(&self.api, out_numel * 8)?;
            allocs.push((d_out, c_out));

            let mut dev_in_ptrs = Vec::with_capacity(input_flats.len());
            for flat in input_flats {
                let (p, c) = Self::upload_slice(&self.api, &mut st.arena, flat)?;
                allocs.push((p, c));
                dev_in_ptrs.push(p);
            }
            let (mut d_in_ptrs, c_ip) = Self::upload_slice(&self.api, &mut st.arena, &dev_in_ptrs)?;
            allocs.push((d_in_ptrs, c_ip));
            let (mut d_strides, c_st) = Self::upload_slice(&self.api, &mut st.arena, &all_strides)?;
            allocs.push((d_strides, c_st));
            let (mut d_oshape, c_os) =
                Self::upload_slice(&self.api, &mut st.arena, &out_shape_u32)?;
            allocs.push((d_oshape, c_os));
            let (mut d_ops, c_ops) = Self::upload_slice(&self.api, &mut st.arena, &ops)?;
            allocs.push((d_ops, c_ops));
            let (mut d_a, c_a) = Self::upload_slice(&self.api, &mut st.arena, &arg_a)?;
            allocs.push((d_a, c_a));
            let (mut d_b, c_b) = Self::upload_slice(&self.api, &mut st.arena, &arg_b)?;
            allocs.push((d_b, c_b));
            let (mut d_imm, c_imm) = Self::upload_slice(&self.api, &mut st.arena, &imm)?;
            allocs.push((d_imm, c_imm));

            let mut ndim_u = ndim as u32;
            let mut n_in_u = input_flats.len() as u32;
            let mut n_st_u = steps.len() as u32;
            let mut numel_u = out_numel as u32;

            let mut params: [*mut c_void; 12] = [
                &raw mut d_out as *mut c_void,
                &raw mut d_in_ptrs as *mut c_void,
                &raw mut d_strides as *mut c_void,
                &raw mut d_oshape as *mut c_void,
                &raw mut ndim_u as *mut c_void,
                &raw mut n_in_u as *mut c_void,
                &raw mut d_ops as *mut c_void,
                &raw mut d_a as *mut c_void,
                &raw mut d_b as *mut c_void,
                &raw mut d_imm as *mut c_void,
                &raw mut n_st_u as *mut c_void,
                &raw mut numel_u as *mut c_void,
            ];
            let blocks = numel_u.div_ceil(128).max(1);
            // SAFETY: Parameter layout matches `mora_fused_ew_f64` PTX signature.
            let rc_ew = unsafe {
                (self.api.cu_launch_kernel)(
                    st.kernels.fused_ew_f64,
                    blocks,
                    1,
                    1,
                    128,
                    1,
                    1,
                    0,
                    ptr::null_mut(),
                    params.as_mut_ptr(),
                    ptr::null_mut(),
                )
            };
            cu_check(rc_ew, "cuLaunchKernel(mora_fused_ew_f64)")?;
            // SAFETY: Synchronize default stream before D2H copy.
            let rc_sync = unsafe { (self.api.cu_ctx_synchronize)() };
            cu_check(rc_sync, "cuCtxSynchronize(mora_fused_ew_f64)")?;
            Self::download_slice(&self.api, d_out, out)?;
            Ok(())
        })();

        for (p, c) in allocs {
            st.arena.release(&self.api, p, c);
        }
        res?;
        quantize_slice_in_place(out, dtype);
        Ok(())
    }
}

/// Dynamically probe all available NVIDIA CUDA GPUs on this machine via the CUDA Driver API.
pub(crate) fn probe_cuda_backends() -> Vec<HardwareBackendEntry> {
    let Some(api) = CudaDriverApi::load() else {
        return Vec::new();
    };
    // SAFETY: `cuInit(0)` initializes the CUDA Driver API once per process.
    if unsafe { (api.cu_init)(0) } != CUDA_SUCCESS {
        return Vec::new();
    }
    let mut driver_ver: c_int = 0;
    // SAFETY: Valid pointer to `c_int`.
    let _ = unsafe { (api.cu_driver_get_version)(&mut driver_ver) };
    let driver_version = format!("{}.{}", driver_ver / 1000, (driver_ver % 1000) / 10);

    let mut count: c_int = 0;
    // SAFETY: Valid pointer to `c_int`.
    if unsafe { (api.cu_device_get_count)(&mut count) } != CUDA_SUCCESS || count <= 0 {
        return Vec::new();
    }

    let mut out = Vec::new();
    for idx in 0..count {
        let mut cu_dev: CuDevice = 0;
        // SAFETY: `idx` is within `0..count`.
        if unsafe { (api.cu_device_get)(&mut cu_dev, idx) } != CUDA_SUCCESS {
            continue;
        }
        let mut name_buf = [0 as c_char; 256];
        // SAFETY: `name_buf` is a 256-byte writable buffer.
        let gpu_name = if unsafe {
            (api.cu_device_get_name)(name_buf.as_mut_ptr(), name_buf.len() as c_int, cu_dev)
        } == CUDA_SUCCESS
        {
            // SAFETY: `cuDeviceGetName` NUL-terminates within `name_buf`.
            unsafe { CStr::from_ptr(name_buf.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        } else {
            format!("CUDA GPU #{idx}")
        };

        let mut total_vram: usize = 0;
        // SAFETY: Valid pointer to `usize`.
        let _ = unsafe { (api.cu_device_total_mem)(&mut total_vram, cu_dev) };

        let mut sm_major: c_int = 0;
        let mut sm_minor: c_int = 0;
        let mut mp_count: c_int = 0;
        // SAFETY: Standard `CUdevice_attribute` integer codes (`cuda.h`).
        unsafe {
            let _ = (api.cu_device_get_attribute)(
                &mut sm_major,
                CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MAJOR,
                cu_dev,
            );
            let _ = (api.cu_device_get_attribute)(
                &mut sm_minor,
                CU_DEVICE_ATTRIBUTE_COMPUTE_CAPABILITY_MINOR,
                cu_dev,
            );
            let _ = (api.cu_device_get_attribute)(
                &mut mp_count,
                CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT,
                cu_dev,
            );
        }

        let mut ctx: CuContext = ptr::null_mut();
        // SAFETY: Retains the shared primary CUDA context for `cu_dev`.
        if unsafe { (api.cu_primary_ctx_retain)(&mut ctx, cu_dev) } != CUDA_SUCCESS || ctx.is_null()
        {
            continue;
        }
        // SAFETY: Bind `ctx` to compile PTX kernels for this device.
        if unsafe { (api.cu_ctx_set_current)(ctx) } != CUDA_SUCCESS {
            // SAFETY: Release retained primary context on failure.
            unsafe {
                let _ = (api.cu_primary_ctx_release)(cu_dev);
            }
            continue;
        }

        let kernels = match compile_cuda_kernels(&api) {
            Ok(k) => k,
            Err(_) => {
                // SAFETY: Release retained primary context if PTX compilation fails.
                unsafe {
                    let _ = (api.cu_primary_ctx_release)(cu_dev);
                }
                continue;
            }
        };

        let device = Device::Cuda(idx as u16);
        let info = GpuDeviceInfo {
            device: device.clone(),
            driver: "cuda".to_string(),
            name: gpu_name,
            total_vram_bytes: total_vram as u64,
            compute_capability: format!("sm_{sm_major}{sm_minor}"),
            multiprocessors: mp_count.max(1) as u32,
            hardware_accelerated: true,
            driver_version: driver_version.clone(),
        };

        let backend = Arc::new(CudaDeviceBackend {
            device,
            cu_dev,
            api: Arc::clone(&api),
            info: info.clone(),
            state: Mutex::new(CudaContextState {
                ctx,
                kernels,
                arena: CudaVramArena::new(),
            }),
        });
        out.push(HardwareBackendEntry { info, backend });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cuda_ptx_jit_compiles_cleanly_when_driver_present() {
        let Some(api) = CudaDriverApi::load() else {
            return;
        };
        // SAFETY: Initialize CUDA Driver API.
        if unsafe { (api.cu_init)(0) } != CUDA_SUCCESS {
            return;
        }
        let mut count: c_int = 0;
        // SAFETY: Valid pointer to `c_int`.
        if unsafe { (api.cu_device_get_count)(&mut count) } != CUDA_SUCCESS || count <= 0 {
            return;
        }
        let mut cu_dev: CuDevice = 0;
        // SAFETY: Device index 0 is valid when `count > 0`.
        assert_eq!(unsafe { (api.cu_device_get)(&mut cu_dev, 0) }, CUDA_SUCCESS);
        let mut ctx: CuContext = ptr::null_mut();
        // SAFETY: Retain primary context for device 0.
        assert_eq!(
            unsafe { (api.cu_primary_ctx_retain)(&mut ctx, cu_dev) },
            CUDA_SUCCESS
        );
        // SAFETY: Bind primary context to current thread.
        assert_eq!(unsafe { (api.cu_ctx_set_current)(ctx) }, CUDA_SUCCESS);

        let res = compile_cuda_kernels(&api);
        if let Ok(k) = &res {
            // SAFETY: Unload compiled module.
            unsafe {
                let _ = (api.cu_module_unload)(k.module);
            }
        }
        // SAFETY: Release primary context.
        unsafe {
            let _ = (api.cu_primary_ctx_release)(cu_dev);
        }
        assert!(
            res.is_ok(),
            "NVIDIA PTX JIT failed to compile MORA_CUDA_PTX:\n{}",
            res.err().map(|e| e.to_string()).unwrap_or_default()
        );
    }
}
