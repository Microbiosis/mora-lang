//! v0.110.0: Native NVIDIA CUDA Driver API (`nvcuda.dll` / `libcuda.so.1`) Hardware Backend.
//!
//! Architecture:
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

use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::fmt;
use std::ptr;
use std::sync::Arc;

use crate::common::sync::Mutex;

use crate::common::{
    DType, Device, bf16_bits_to_f64, f16_bits_to_f64, f64_to_bf16_bits, f64_to_f16_bits,
};
use crate::error::{MoraError, MoraResult};
use crate::runtime::compute_dag::{
    DeviceBackend, DeviceVramBuffer, FusedAct, FusedEwStep, VramTelemetry,
};
use crate::runtime::gpu::{DynLib, GpuDeviceInfo, HardwareBackendEntry, VramCounters};
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
    matmul_f32: CuFunction,
    matmul_f16: CuFunction,
    matmul_bf16: CuFunction,
    fused_linear_act_f64: CuFunction,
    fused_linear_act_f32: CuFunction,
    fused_linear_act_f16: CuFunction,
    fused_linear_act_bf16: CuFunction,
    rms_inv_f64: CuFunction,
    fused_rms_linear_act_f64: CuFunction,
    fused_flash_attention_f64: CuFunction,
    fused_ew_f64: CuFunction,
    cast_to_f64: CuFunction,
    cast_from_f64: CuFunction,
}

struct CudaContextState {
    ctx: CuContext,
    kernels: CudaKernels,
    arena: CudaVramArena,
    /// P4-1B/P6-3B: Cached compiled user-defined GPU `kernel` modules keyed by
    /// `(KernelProgram::content_hash(), block_key)` (block geometry specialization when shared memory is used).
    custom_modules: HashMap<(u64, [u32; 3]), (CuModule, CuFunction)>,
}

struct CudaDeviceInner {
    device: Device,
    cu_dev: CuDevice,
    api: Arc<CudaDriverApi>,
    /// Serializes context binding + arena/kernel launch safely across concurrent Mora `spawn` tasks.
    state: Mutex<CudaContextState>,
    counters: VramCounters,
}

// SAFETY: `CudaDeviceInner` guards its `CUcontext`, `CUmodule`, `CUfunction`s, and `CudaVramArena`
// inside a `Mutex`, and binds `cuCtxSetCurrent` on every locked operation before calling CUDA APIs.
unsafe impl Send for CudaDeviceInner {}
// SAFETY: All CUDA operations occur strictly under `self.state.lock()` with `cuCtxSetCurrent`.
unsafe impl Sync for CudaDeviceInner {}

impl Drop for CudaDeviceInner {
    fn drop(&mut self) {
        let mut st = self.state.lock();
        // SAFETY: Context is valid until `cuDevicePrimaryCtxRelease_v2` below; all `CudaVramBuffer`s
        // holding `Arc<CudaDeviceInner>` have already been dropped before this runs.
        unsafe {
            let _ = (self.api.cu_ctx_set_current)(st.ctx);
            st.arena.drain(&self.api);
            for (_, (cmod, _)) in st.custom_modules.drain() {
                if !cmod.is_null() {
                    let _ = (self.api.cu_module_unload)(cmod);
                }
            }
            if !st.kernels.module.is_null() {
                let _ = (self.api.cu_module_unload)(st.kernels.module);
                st.kernels.module = ptr::null_mut();
            }
            let _ = (self.api.cu_primary_ctx_release)(self.cu_dev);
        }
    }
}

/// Owned physical CUDA VRAM buffer (`CUdeviceptr`) with RAII return to `CudaVramArena`.
struct CudaVramBuffer {
    inner: Arc<CudaDeviceInner>,
    ptr: CuDevicePtr,
    cap: usize,
    byte_len: usize,
}

impl fmt::Debug for CudaVramBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CudaVramBuffer")
            .field("device", &self.inner.device)
            .field("ptr", &format_args!("0x{:x}", self.ptr))
            .field("byte_len", &self.byte_len)
            .finish()
    }
}

impl Drop for CudaVramBuffer {
    fn drop(&mut self) {
        let mut st = self.inner.state.lock();
        // SAFETY: Bind retained primary context before releasing `CUdeviceptr` to arena.
        let _ = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        st.arena.release(&self.inner.api, self.ptr, self.cap);
        self.inner.counters.on_free(self.byte_len);
    }
}

impl DeviceVramBuffer for CudaVramBuffer {
    fn device(&self) -> &Device {
        &self.inner.device
    }

    fn byte_len(&self) -> usize {
        self.byte_len
    }

    fn raw_handle(&self) -> u64 {
        self.ptr
    }

    fn upload_bytes(&self, src: &[u8]) -> MoraResult<()> {
        if src.len() != self.byte_len {
            return Err(MoraError::Runtime(format!(
                "CudaVramBuffer::upload_bytes: expected {} bytes, got {}",
                self.byte_len,
                src.len()
            )));
        }
        if src.is_empty() {
            return Ok(());
        }
        let st = self.inner.state.lock();
        // SAFETY: Bind retained primary CUDA context before H2D copy.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent(upload_bytes)")?;
        // SAFETY: `self.ptr` has capacity `self.cap >= self.byte_len == src.len()`.
        let rc = unsafe {
            (self.inner.api.cu_memcpy_htod)(self.ptr, src.as_ptr() as *const c_void, src.len())
        };
        cu_check(rc, "cuMemcpyHtoD_v2(upload_bytes)")?;
        self.inner.counters.on_h2d();
        Ok(())
    }

    fn download_bytes(&self, dst: &mut [u8]) -> MoraResult<()> {
        if dst.len() != self.byte_len {
            return Err(MoraError::Runtime(format!(
                "CudaVramBuffer::download_bytes: expected {} bytes, got {}",
                self.byte_len,
                dst.len()
            )));
        }
        if dst.is_empty() {
            return Ok(());
        }
        let st = self.inner.state.lock();
        // SAFETY: Bind retained primary CUDA context before D2H copy.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent(download_bytes)")?;
        // SAFETY: `self.ptr` has exact initialized `self.byte_len == dst.len()` bytes.
        let rc = unsafe {
            (self.inner.api.cu_memcpy_dtoh)(dst.as_mut_ptr() as *mut c_void, self.ptr, dst.len())
        };
        cu_check(rc, "cuMemcpyDtoH_v2(download_bytes)")?;
        self.inner.counters.on_d2h();
        Ok(())
    }
}

/// Live NVIDIA CUDA hardware [`DeviceBackend`] executing native PTX ISA 7.0 `f64`/`f32`/`f16`/`bf16`
/// kernels directly on persistent GPU VRAM.
pub(crate) struct CudaDeviceBackend {
    inner: Arc<CudaDeviceInner>,
    info: GpuDeviceInfo,
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
    setp.eq.u32 %p0, %r10, 24; @%p0 bra OP_ABS;
    setp.eq.u32 %p0, %r10, 25; @%p0 bra OP_STEPPOS;
    bra OP_SIGN;

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
OP_ABS: abs.f64 %fd3, %fd0; bra EW_STORE_SLOT;
OP_STEPPOS:
    setp.gt.f64 %p0, %fd0, 0d0000000000000000;
    selp.f64 %fd3, 0d3FF0000000000000, 0d0000000000000000, %p0;
    bra EW_STORE_SLOT;
OP_SIGN:
    setp.gt.f64 %p0, %fd0, 0d0000000000000000;
    selp.f64 %fd3, 0d3FF0000000000000, 0d0000000000000000, %p0;
    setp.lt.f64 %p0, %fd0, 0d0000000000000000;
    selp.f64 %fd3, 0dBFF0000000000000, %fd3, %p0;

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

// Helper: f32 activation evaluated via high-accuracy f64 transcendental path
.func (.reg .f32 res) mora_apply_act_f32(.reg .f32 x, .reg .u32 act)
{
    .reg .f64 %fd0, %fd1;
    cvt.f64.f32 %fd0, x;
    call (%fd1), mora_apply_act, (%fd0, act);
    cvt.rn.f32.f64 res, %fd1;
    ret;
}

// Helper: BF16 (low 16 bits in .u32) -> F32 (.f32) exact bit-shift
.func (.reg .f32 res) mora_bf16_to_f32(.reg .u32 h)
{
    .reg .u32 %r0;
    shl.b32 %r0, h, 16;
    mov.b32 res, %r0;
    ret;
}

// Helper: F32 (.f32) -> BF16 (low 16 bits in .u32) with IEEE round-to-nearest-even & NaN preservation
.func (.reg .u32 res) mora_f32_to_bf16(.reg .f32 x)
{
    .reg .pred %p0;
    .reg .u32 %r<6>;
    mov.b32 %r0, x;
    and.b32 %r1, %r0, 0x7fffffff;
    setp.gt.u32 %p0, %r1, 0x7f800000;
    @%p0 bra BF16_NAN;
    shr.b32 %r2, %r0, 16;
    and.b32 %r2, %r2, 1;
    add.u32 %r3, %r2, 0x7fff;
    add.u32 %r4, %r0, %r3;
    shr.b32 res, %r4, 16;
    ret;
BF16_NAN:
    shr.b32 %r5, %r0, 16;
    or.b32 res, %r5, 0x0040;
    ret;
}

// Kernel 6: Native f32 Batched/2D MatMul directly on 4-byte VRAM buffers.
.visible .entry mora_matmul_f32(
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
    .reg .f32 %f<4>;

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
    @!%p3 bra MM32_EXIT;

    cvt.u64.u32 %rd5, %r9;
    shl.b64 %rd6, %rd5, 3;
    add.u64 %rd7, %rd3, %rd6;
    ld.global.u64 %rd8, [%rd7];
    add.u64 %rd7, %rd4, %rd6;
    ld.global.u64 %rd9, [%rd7];

    mul.wide.u32 %rd10, %r8, %r3;
    add.u64 %rd8, %rd8, %rd10;

    mov.f32 %f0, 0f00000000;
    mov.u32 %r10, 0;

MM32_LOOP:
    setp.ge.u32 %p0, %r10, %r3;
    @%p0 bra MM32_DONE;

    cvt.u64.u32 %rd11, %r10;
    add.u64 %rd12, %rd8, %rd11;
    shl.b64 %rd12, %rd12, 2;
    add.u64 %rd12, %rd1, %rd12;
    ld.global.f32 %f1, [%rd12];

    mul.wide.u32 %rd13, %r10, %r2;
    add.u64 %rd13, %rd13, %rd9;
    cvt.u64.u32 %rd14, %r7;
    add.u64 %rd13, %rd13, %rd14;
    shl.b64 %rd13, %rd13, 2;
    add.u64 %rd13, %rd2, %rd13;
    ld.global.f32 %f2, [%rd13];

    fma.rn.f32 %f0, %f1, %f2, %f0;
    add.u32 %r10, %r10, 1;
    bra MM32_LOOP;

MM32_DONE:
    mul.wide.u32 %rd11, %r1, %r2;
    mul.lo.u64 %rd12, %rd5, %rd11;
    mul.wide.u32 %rd13, %r8, %r2;
    add.u64 %rd12, %rd12, %rd13;
    cvt.u64.u32 %rd14, %r7;
    add.u64 %rd12, %rd12, %rd14;
    shl.b64 %rd12, %rd12, 2;
    add.u64 %rd12, %rd0, %rd12;
    st.global.f32 [%rd12], %f0;

MM32_EXIT:
    ret;
}

// Kernel 7: Native f32 Fused Linear + optional Bias + Activation directly on 4-byte VRAM buffers.
.visible .entry mora_fused_linear_act_f32(
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
    .reg .f32 %f<5>;

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
    @!%p2 bra FLA32_EXIT;

    mul.wide.u32 %rd4, %r10, %r3;
    mov.f32 %f0, 0f00000000;
    mov.u32 %r11, 0;

FLA32_LOOP:
    setp.ge.u32 %p0, %r11, %r3;
    @%p0 bra FLA32_AFTER_DOT;

    cvt.u64.u32 %rd5, %r11;
    add.u64 %rd6, %rd4, %rd5;
    shl.b64 %rd6, %rd6, 2;
    add.u64 %rd6, %rd1, %rd6;
    ld.global.f32 %f1, [%rd6];

    mul.wide.u32 %rd7, %r11, %r4;
    cvt.u64.u32 %rd8, %r9;
    add.u64 %rd7, %rd7, %rd8;
    shl.b64 %rd7, %rd7, 2;
    add.u64 %rd7, %rd2, %rd7;
    ld.global.f32 %f2, [%rd7];

    fma.rn.f32 %f0, %f1, %f2, %f0;
    add.u32 %r11, %r11, 1;
    bra FLA32_LOOP;

FLA32_AFTER_DOT:
    setp.eq.u32 %p0, %r0, 0;
    @%p0 bra FLA32_ACT;
    rem.u32 %r12, %r9, %r1;
    cvt.u64.u32 %rd5, %r12;
    shl.b64 %rd5, %rd5, 2;
    add.u64 %rd5, %rd3, %rd5;
    ld.global.f32 %f3, [%rd5];
    add.rn.f32 %f0, %f0, %f3;

FLA32_ACT:
    call (%f4), mora_apply_act_f32, (%f0, %r5);

    mul.wide.u32 %rd9, %r10, %r4;
    cvt.u64.u32 %rd10, %r9;
    add.u64 %rd9, %rd9, %rd10;
    shl.b64 %rd9, %rd9, 2;
    add.u64 %rd9, %rd0, %rd9;
    st.global.f32 [%rd9], %f4;

FLA32_EXIT:
    ret;
}

// Kernel 8: Native FP16 (IEEE-754 binary16, 2-byte VRAM) Batched/2D MatMul with f32 accumulation.
.visible .entry mora_matmul_f16(
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
    .reg .b16 %h<4>;
    .reg .f32 %f<4>;

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
    @!%p3 bra MM16_EXIT;

    cvt.u64.u32 %rd5, %r9;
    shl.b64 %rd6, %rd5, 3;
    add.u64 %rd7, %rd3, %rd6;
    ld.global.u64 %rd8, [%rd7];
    add.u64 %rd7, %rd4, %rd6;
    ld.global.u64 %rd9, [%rd7];

    mul.wide.u32 %rd10, %r8, %r3;
    add.u64 %rd8, %rd8, %rd10;

    mov.f32 %f0, 0f00000000;
    mov.u32 %r10, 0;

MM16_LOOP:
    setp.ge.u32 %p0, %r10, %r3;
    @%p0 bra MM16_DONE;

    cvt.u64.u32 %rd11, %r10;
    add.u64 %rd12, %rd8, %rd11;
    shl.b64 %rd12, %rd12, 1;
    add.u64 %rd12, %rd1, %rd12;
    ld.global.b16 %h0, [%rd12];
    cvt.f32.f16 %f1, %h0;

    mul.wide.u32 %rd13, %r10, %r2;
    add.u64 %rd13, %rd13, %rd9;
    cvt.u64.u32 %rd14, %r7;
    add.u64 %rd13, %rd13, %rd14;
    shl.b64 %rd13, %rd13, 1;
    add.u64 %rd13, %rd2, %rd13;
    ld.global.b16 %h1, [%rd13];
    cvt.f32.f16 %f2, %h1;

    fma.rn.f32 %f0, %f1, %f2, %f0;
    add.u32 %r10, %r10, 1;
    bra MM16_LOOP;

MM16_DONE:
    cvt.rn.f16.f32 %h2, %f0;
    mul.wide.u32 %rd11, %r1, %r2;
    mul.lo.u64 %rd12, %rd5, %rd11;
    mul.wide.u32 %rd13, %r8, %r2;
    add.u64 %rd12, %rd12, %rd13;
    cvt.u64.u32 %rd14, %r7;
    add.u64 %rd12, %rd12, %rd14;
    shl.b64 %rd12, %rd12, 1;
    add.u64 %rd12, %rd0, %rd12;
    st.global.b16 [%rd12], %h2;

MM16_EXIT:
    ret;
}

// Kernel 9: Native FP16 Fused Linear + optional Bias + Activation on 2-byte VRAM buffers.
.visible .entry mora_fused_linear_act_f16(
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
    .reg .b16 %h<4>;
    .reg .f32 %f<5>;

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
    @!%p2 bra FLA16_EXIT;

    mul.wide.u32 %rd4, %r10, %r3;
    mov.f32 %f0, 0f00000000;
    mov.u32 %r11, 0;

FLA16_LOOP:
    setp.ge.u32 %p0, %r11, %r3;
    @%p0 bra FLA16_AFTER_DOT;

    cvt.u64.u32 %rd5, %r11;
    add.u64 %rd6, %rd4, %rd5;
    shl.b64 %rd6, %rd6, 1;
    add.u64 %rd6, %rd1, %rd6;
    ld.global.b16 %h0, [%rd6];
    cvt.f32.f16 %f1, %h0;

    mul.wide.u32 %rd7, %r11, %r4;
    cvt.u64.u32 %rd8, %r9;
    add.u64 %rd7, %rd7, %rd8;
    shl.b64 %rd7, %rd7, 1;
    add.u64 %rd7, %rd2, %rd7;
    ld.global.b16 %h1, [%rd7];
    cvt.f32.f16 %f2, %h1;

    fma.rn.f32 %f0, %f1, %f2, %f0;
    add.u32 %r11, %r11, 1;
    bra FLA16_LOOP;

FLA16_AFTER_DOT:
    setp.eq.u32 %p0, %r0, 0;
    @%p0 bra FLA16_ACT;
    rem.u32 %r12, %r9, %r1;
    cvt.u64.u32 %rd5, %r12;
    shl.b64 %rd5, %rd5, 1;
    add.u64 %rd5, %rd3, %rd5;
    ld.global.b16 %h2, [%rd5];
    cvt.f32.f16 %f3, %h2;
    add.rn.f32 %f0, %f0, %f3;

FLA16_ACT:
    call (%f4), mora_apply_act_f32, (%f0, %r5);
    cvt.rn.f16.f32 %h3, %f4;

    mul.wide.u32 %rd9, %r10, %r4;
    cvt.u64.u32 %rd10, %r9;
    add.u64 %rd9, %rd9, %rd10;
    shl.b64 %rd9, %rd9, 1;
    add.u64 %rd9, %rd0, %rd9;
    st.global.b16 [%rd9], %h3;

FLA16_EXIT:
    ret;
}

// Kernel 10: Native BF16 (2-byte VRAM) Batched/2D MatMul with f32 accumulation & RNE rounding.
.visible .entry mora_matmul_bf16(
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
    .reg .u16 %u<4>;
    .reg .f32 %f<4>;

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
    @!%p3 bra MMBF_EXIT;

    cvt.u64.u32 %rd5, %r9;
    shl.b64 %rd6, %rd5, 3;
    add.u64 %rd7, %rd3, %rd6;
    ld.global.u64 %rd8, [%rd7];
    add.u64 %rd7, %rd4, %rd6;
    ld.global.u64 %rd9, [%rd7];

    mul.wide.u32 %rd10, %r8, %r3;
    add.u64 %rd8, %rd8, %rd10;

    mov.f32 %f0, 0f00000000;
    mov.u32 %r10, 0;

MMBF_LOOP:
    setp.ge.u32 %p0, %r10, %r3;
    @%p0 bra MMBF_DONE;

    cvt.u64.u32 %rd11, %r10;
    add.u64 %rd12, %rd8, %rd11;
    shl.b64 %rd12, %rd12, 1;
    add.u64 %rd12, %rd1, %rd12;
    ld.global.u16 %u0, [%rd12];
    cvt.u32.u16 %r11, %u0;
    call (%f1), mora_bf16_to_f32, (%r11);

    mul.wide.u32 %rd13, %r10, %r2;
    add.u64 %rd13, %rd13, %rd9;
    cvt.u64.u32 %rd14, %r7;
    add.u64 %rd13, %rd13, %rd14;
    shl.b64 %rd13, %rd13, 1;
    add.u64 %rd13, %rd2, %rd13;
    ld.global.u16 %u1, [%rd13];
    cvt.u32.u16 %r12, %u1;
    call (%f2), mora_bf16_to_f32, (%r12);

    fma.rn.f32 %f0, %f1, %f2, %f0;
    add.u32 %r10, %r10, 1;
    bra MMBF_LOOP;

MMBF_DONE:
    call (%r13), mora_f32_to_bf16, (%f0);
    cvt.u16.u32 %u2, %r13;
    mul.wide.u32 %rd11, %r1, %r2;
    mul.lo.u64 %rd12, %rd5, %rd11;
    mul.wide.u32 %rd13, %r8, %r2;
    add.u64 %rd12, %rd12, %rd13;
    cvt.u64.u32 %rd14, %r7;
    add.u64 %rd12, %rd12, %rd14;
    shl.b64 %rd12, %rd12, 1;
    add.u64 %rd12, %rd0, %rd12;
    st.global.u16 [%rd12], %u2;

MMBF_EXIT:
    ret;
}

// Kernel 11: Native BF16 Fused Linear + optional Bias + Activation on 2-byte VRAM buffers.
.visible .entry mora_fused_linear_act_bf16(
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
    .reg .u32 %r<16>;
    .reg .u64 %rd<12>;
    .reg .u16 %u<4>;
    .reg .f32 %f<5>;

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
    @!%p2 bra FLABF_EXIT;

    mul.wide.u32 %rd4, %r10, %r3;
    mov.f32 %f0, 0f00000000;
    mov.u32 %r11, 0;

FLABF_LOOP:
    setp.ge.u32 %p0, %r11, %r3;
    @%p0 bra FLABF_AFTER_DOT;

    cvt.u64.u32 %rd5, %r11;
    add.u64 %rd6, %rd4, %rd5;
    shl.b64 %rd6, %rd6, 1;
    add.u64 %rd6, %rd1, %rd6;
    ld.global.u16 %u0, [%rd6];
    cvt.u32.u16 %r13, %u0;
    call (%f1), mora_bf16_to_f32, (%r13);

    mul.wide.u32 %rd7, %r11, %r4;
    cvt.u64.u32 %rd8, %r9;
    add.u64 %rd7, %rd7, %rd8;
    shl.b64 %rd7, %rd7, 1;
    add.u64 %rd7, %rd2, %rd7;
    ld.global.u16 %u1, [%rd7];
    cvt.u32.u16 %r14, %u1;
    call (%f2), mora_bf16_to_f32, (%r14);

    fma.rn.f32 %f0, %f1, %f2, %f0;
    add.u32 %r11, %r11, 1;
    bra FLABF_LOOP;

FLABF_AFTER_DOT:
    setp.eq.u32 %p0, %r0, 0;
    @%p0 bra FLABF_ACT;
    rem.u32 %r12, %r9, %r1;
    cvt.u64.u32 %rd5, %r12;
    shl.b64 %rd5, %rd5, 1;
    add.u64 %rd5, %rd3, %rd5;
    ld.global.u16 %u2, [%rd5];
    cvt.u32.u16 %r13, %u2;
    call (%f3), mora_bf16_to_f32, (%r13);
    add.rn.f32 %f0, %f0, %f3;

FLABF_ACT:
    call (%f4), mora_apply_act_f32, (%f0, %r5);
    call (%r15), mora_f32_to_bf16, (%f4);
    cvt.u16.u32 %u3, %r15;

    mul.wide.u32 %rd9, %r10, %r4;
    cvt.u64.u32 %rd10, %r9;
    add.u64 %rd9, %rd9, %rd10;
    shl.b64 %rd9, %rd9, 1;
    add.u64 %rd9, %rd0, %rd9;
    st.global.u16 [%rd9], %u3;

FLABF_EXIT:
    ret;
}

// -- On-device VRAM DType conversion kernels (0 PCIe H2D/D2H for f32/f16/bf16 VRAM pipelines) --
// fmt: 0 = f32, 1 = f16, 2 = bf16
.visible .entry mora_cast_to_f64(
    .param .u64 p_dst,
    .param .u64 p_src,
    .param .u32 p_n,
    .param .u32 p_fmt
)
{
    .reg .pred %p<4>;
    .reg .u32 %r<8>;
    .reg .u16 %u<2>;
    .reg .b16 %h<2>;
    .reg .u64 %rd<8>;
    .reg .f32 %f<2>;
    .reg .f64 %fd<2>;

    ld.param.u64 %rd0, [p_dst];
    ld.param.u64 %rd1, [p_src];
    ld.param.u32 %r0, [p_n];
    ld.param.u32 %r1, [p_fmt];

    mov.u32 %r2, %ctaid.x;
    mov.u32 %r3, %ntid.x;
    mov.u32 %r4, %tid.x;
    mad.lo.u32 %r5, %r2, %r3, %r4;
    setp.ge.u32 %p0, %r5, %r0;
    @%p0 bra C2F64_EXIT;

    cvt.u64.u32 %rd2, %r5;
    setp.eq.u32 %p1, %r1, 0;
    @%p1 bra C2F64_F32;
    setp.eq.u32 %p2, %r1, 1;
    @%p2 bra C2F64_F16;

    // bf16 -> f64
    shl.b64 %rd3, %rd2, 1;
    add.u64 %rd3, %rd1, %rd3;
    ld.global.u16 %u0, [%rd3];
    cvt.u32.u16 %r6, %u0;
    call (%f0), mora_bf16_to_f32, (%r6);
    cvt.f64.f32 %fd0, %f0;
    bra C2F64_STORE;

C2F64_F32:
    shl.b64 %rd3, %rd2, 2;
    add.u64 %rd3, %rd1, %rd3;
    ld.global.f32 %f0, [%rd3];
    cvt.f64.f32 %fd0, %f0;
    bra C2F64_STORE;

C2F64_F16:
    shl.b64 %rd3, %rd2, 1;
    add.u64 %rd3, %rd1, %rd3;
    ld.global.b16 %h0, [%rd3];
    cvt.f32.f16 %f0, %h0;
    cvt.f64.f32 %fd0, %f0;

C2F64_STORE:
    shl.b64 %rd4, %rd2, 3;
    add.u64 %rd4, %rd0, %rd4;
    st.global.f64 [%rd4], %fd0;

C2F64_EXIT:
    ret;
}

.visible .entry mora_cast_from_f64(
    .param .u64 p_dst,
    .param .u64 p_src,
    .param .u32 p_n,
    .param .u32 p_fmt
)
{
    .reg .pred %p<4>;
    .reg .u32 %r<8>;
    .reg .u16 %u<2>;
    .reg .b16 %h<2>;
    .reg .u64 %rd<8>;
    .reg .f32 %f<2>;
    .reg .f64 %fd<2>;

    ld.param.u64 %rd0, [p_dst];
    ld.param.u64 %rd1, [p_src];
    ld.param.u32 %r0, [p_n];
    ld.param.u32 %r1, [p_fmt];

    mov.u32 %r2, %ctaid.x;
    mov.u32 %r3, %ntid.x;
    mov.u32 %r4, %tid.x;
    mad.lo.u32 %r5, %r2, %r3, %r4;
    setp.ge.u32 %p0, %r5, %r0;
    @%p0 bra CF64_EXIT;

    cvt.u64.u32 %rd2, %r5;
    shl.b64 %rd3, %rd2, 3;
    add.u64 %rd3, %rd1, %rd3;
    ld.global.f64 %fd0, [%rd3];
    cvt.rn.f32.f64 %f0, %fd0;

    setp.eq.u32 %p1, %r1, 0;
    @%p1 bra CF64_F32;
    setp.eq.u32 %p2, %r1, 1;
    @%p2 bra CF64_F16;

    // f64 -> bf16
    call (%r6), mora_f32_to_bf16, (%f0);
    cvt.u16.u32 %u0, %r6;
    shl.b64 %rd4, %rd2, 1;
    add.u64 %rd4, %rd0, %rd4;
    st.global.u16 [%rd4], %u0;
    bra CF64_EXIT;

CF64_F32:
    shl.b64 %rd4, %rd2, 2;
    add.u64 %rd4, %rd0, %rd4;
    st.global.f32 [%rd4], %f0;
    bra CF64_EXIT;

CF64_F16:
    cvt.rn.f16.f32 %h0, %f0;
    shl.b64 %rd4, %rd2, 1;
    add.u64 %rd4, %rd0, %rd4;
    st.global.b16 [%rd4], %h0;

CF64_EXIT:
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
        matmul_f32: load_kernel_fn(api, module, "mora_matmul_f32")?,
        matmul_f16: load_kernel_fn(api, module, "mora_matmul_f16")?,
        matmul_bf16: load_kernel_fn(api, module, "mora_matmul_bf16")?,
        fused_linear_act_f64: load_kernel_fn(api, module, "mora_fused_linear_act_f64")?,
        fused_linear_act_f32: load_kernel_fn(api, module, "mora_fused_linear_act_f32")?,
        fused_linear_act_f16: load_kernel_fn(api, module, "mora_fused_linear_act_f16")?,
        fused_linear_act_bf16: load_kernel_fn(api, module, "mora_fused_linear_act_bf16")?,
        rms_inv_f64: load_kernel_fn(api, module, "mora_rms_inv_f64")?,
        fused_rms_linear_act_f64: load_kernel_fn(api, module, "mora_fused_rms_linear_act_f64")?,
        fused_flash_attention_f64: load_kernel_fn(api, module, "mora_fused_flash_attention_f64")?,
        fused_ew_f64: load_kernel_fn(api, module, "mora_fused_ew_f64")?,
        cast_to_f64: load_kernel_fn(api, module, "mora_cast_to_f64")?,
        cast_from_f64: load_kernel_fn(api, module, "mora_cast_from_f64")?,
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

    fn alloc_owned_vram(
        &self,
        st: &mut CudaContextState,
        byte_len: usize,
    ) -> MoraResult<Arc<CudaVramBuffer>> {
        let (ptr, cap) = st.arena.alloc(&self.inner.api, byte_len.max(8))?;
        self.inner.counters.on_alloc(byte_len);
        Ok(Arc::new(CudaVramBuffer {
            inner: Arc::clone(&self.inner),
            ptr,
            cap,
            byte_len,
        }))
    }

    fn resolve_or_alloc_vram(
        &self,
        st: &mut CudaContextState,
        byte_len: usize,
        dst: Option<Arc<dyn DeviceVramBuffer>>,
    ) -> MoraResult<Arc<dyn DeviceVramBuffer>> {
        if let Some(buf) = dst
            && buf.device() == &self.inner.device
            && buf.byte_len() >= byte_len
        {
            return Ok(buf);
        }
        Ok(self.alloc_owned_vram(st, byte_len)?)
    }

    #[allow(clippy::too_many_arguments)]
    fn launch_matmul_kernel(
        api: &CudaDriverApi,
        kernel: CuFunction,
        mut d_out: CuDevicePtr,
        mut d_a: CuDevicePtr,
        mut d_b: CuDevicePtr,
        mut d_ab: CuDevicePtr,
        mut d_bb: CuDevicePtr,
        batches: usize,
        m: usize,
        n: usize,
        k: usize,
        label: &str,
    ) -> MoraResult<()> {
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
        let gx = n_u.div_ceil(16).max(1);
        let gy = m_u.div_ceil(16).max(1);
        // SAFETY: Parameter layout matches `mora_matmul_{f64,f32,f16,bf16}` PTX signature exactly.
        let rc = unsafe {
            (api.cu_launch_kernel)(
                kernel,
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
        cu_check(rc, label)?;
        // SAFETY: Synchronize default stream to guarantee completion before readback/reuse.
        let rc_sync = unsafe { (api.cu_ctx_synchronize)() };
        cu_check(rc_sync, label)
    }

    #[allow(clippy::too_many_arguments)]
    fn launch_linear_act_kernel(
        api: &CudaDriverApi,
        kernel: CuFunction,
        mut d_out: CuDevicePtr,
        mut d_x: CuDevicePtr,
        mut d_w: CuDevicePtr,
        mut d_bias: CuDevicePtr,
        mut hb: u32,
        mut bl: u32,
        m_total: usize,
        k: usize,
        n: usize,
        act: FusedAct,
        label: &str,
    ) -> MoraResult<()> {
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
        // SAFETY: Parameter layout matches `mora_fused_linear_act_{f64,f32,f16,bf16}` PTX signature.
        let rc = unsafe {
            (api.cu_launch_kernel)(
                kernel,
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
        cu_check(rc, label)?;
        // SAFETY: Synchronize default stream to guarantee completion before readback/reuse.
        let rc_sync = unsafe { (api.cu_ctx_synchronize)() };
        cu_check(rc_sync, label)
    }

    fn encode_ew_steps(steps: &[FusedEwStep]) -> (Vec<u32>, Vec<u32>, Vec<u32>, Vec<f64>) {
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
                FusedEwStep::StepPos(a) => (25, a as u32, 0, 0.0),
                FusedEwStep::Sign(a) => (26, a as u32, 0, 0.0),
            };
            ops.push(op);
            arg_a.push(a);
            arg_b.push(b);
            imm.push(im);
        }
        (ops, arg_a, arg_b, imm)
    }

    fn cast_fmt_code(dtype: DType) -> Option<u32> {
        match dtype {
            DType::F32 => Some(0),
            DType::F16 => Some(1),
            DType::BF16 => Some(2),
            _ => None,
        }
    }

    /// Ensure a resident VRAM tensor buffer is viewable as `f64` device memory without PCIe transfers.
    fn vram_ptr_as_f64(
        api: &CudaDriverApi,
        st: &mut CudaContextState,
        buf: &dyn DeviceVramBuffer,
        dtype: DType,
        temp_allocs: &mut Vec<(CuDevicePtr, usize)>,
    ) -> MoraResult<CuDevicePtr> {
        if dtype == DType::F64 {
            return Ok(buf.raw_handle());
        }
        let Some(mut fmt) = Self::cast_fmt_code(dtype) else {
            return Err(MoraError::Runtime(format!(
                "CUDA VRAM f64 cast unsupported for dtype {}",
                dtype.short_name()
            )));
        };
        let elem_bytes = dtype.storage_bytes(1).max(1);
        let numel = buf.byte_len() / elem_bytes;
        let (mut d_dst, cap) = st.arena.alloc(api, numel.max(1) * 8)?;
        temp_allocs.push((d_dst, cap));
        if numel == 0 {
            return Ok(d_dst);
        }
        let mut d_src = buf.raw_handle();
        let mut n_u = numel as u32;
        let mut params: [*mut c_void; 4] = [
            &raw mut d_dst as *mut c_void,
            &raw mut d_src as *mut c_void,
            &raw mut n_u as *mut c_void,
            &raw mut fmt as *mut c_void,
        ];
        let blocks = n_u.div_ceil(128).max(1);
        // SAFETY: Parameter layout matches `mora_cast_to_f64` PTX signature exactly.
        let rc = unsafe {
            (api.cu_launch_kernel)(
                st.kernels.cast_to_f64,
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
        cu_check(rc, "cuLaunchKernel(mora_cast_to_f64)")?;
        Ok(d_dst)
    }

    /// Cast an `f64` device buffer directly into a target `f32`/`f16`/`bf16` resident VRAM buffer.
    fn launch_cast_from_f64(
        api: &CudaDriverApi,
        st: &CudaContextState,
        mut d_dst: CuDevicePtr,
        mut d_src: CuDevicePtr,
        numel: usize,
        dtype: DType,
    ) -> MoraResult<()> {
        let Some(mut fmt) = Self::cast_fmt_code(dtype) else {
            return Err(MoraError::Runtime(format!(
                "CUDA VRAM cast-from-f64 unsupported for dtype {}",
                dtype.short_name()
            )));
        };
        if numel == 0 {
            return Ok(());
        }
        let mut n_u = numel as u32;
        let mut params: [*mut c_void; 4] = [
            &raw mut d_dst as *mut c_void,
            &raw mut d_src as *mut c_void,
            &raw mut n_u as *mut c_void,
            &raw mut fmt as *mut c_void,
        ];
        let blocks = n_u.div_ceil(128).max(1);
        // SAFETY: Parameter layout matches `mora_cast_from_f64` PTX signature exactly.
        let rc = unsafe {
            (api.cu_launch_kernel)(
                st.kernels.cast_from_f64,
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
        cu_check(rc, "cuLaunchKernel(mora_cast_from_f64)")
    }
}

impl DeviceBackend for CudaDeviceBackend {
    fn kind(&self) -> Device {
        self.inner.device.clone()
    }

    fn name(&self) -> &str {
        "cuda"
    }

    fn simd_isa(&self) -> &'static str {
        "ptx-sm50-f64/f32/f16/bf16"
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

    fn alloc_vram_buffer(&self, byte_len: usize) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        let mut st = self.inner.state.lock();
        // SAFETY: Bind retained primary CUDA context before allocating VRAM.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent(alloc_vram_buffer)")?;
        let buf = self.alloc_owned_vram(&mut st, byte_len)?;
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
        dst: Option<Arc<dyn DeviceVramBuffer>>,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        if a_shape.len() < 2
            || b_shape.len() < 2
            || !matches!(dtype, DType::F64 | DType::F32 | DType::F16 | DType::BF16)
        {
            return Ok(None);
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
        let out_bytes = dtype.storage_bytes(out_numel);
        if out_numel == 0 || k == 0 {
            return Ok(None);
        }

        let a_bases: Vec<u64> = (0..batches)
            .map(|bi| (map_broadcast_batch(bi, &out_batch, a_batch) * m * k) as u64)
            .collect();
        let b_bases: Vec<u64> = (0..batches)
            .map(|bi| (map_broadcast_batch(bi, &out_batch, b_batch) * k * n) as u64)
            .collect();

        let mut st = self.inner.state.lock();
        // SAFETY: Bind retained primary CUDA context.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent(matmul_vram)")?;

        let out_buf = self.resolve_or_alloc_vram(&mut st, out_bytes, dst)?;
        let mut temp_allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(2);
        let kernel = match dtype {
            DType::F64 => st.kernels.matmul_f64,
            DType::F32 => st.kernels.matmul_f32,
            DType::F16 => st.kernels.matmul_f16,
            DType::BF16 => st.kernels.matmul_bf16,
            _ => unreachable!(),
        };
        let label = match dtype {
            DType::F64 => "mora_matmul_f64(vram)",
            DType::F32 => "mora_matmul_f32(vram)",
            DType::F16 => "mora_matmul_f16(vram)",
            DType::BF16 => "mora_matmul_bf16(vram)",
            _ => unreachable!(),
        };

        let res = (|| -> MoraResult<()> {
            let (d_ab, c_ab) = Self::upload_slice(&self.inner.api, &mut st.arena, &a_bases)?;
            temp_allocs.push((d_ab, c_ab));
            let (d_bb, c_bb) = Self::upload_slice(&self.inner.api, &mut st.arena, &b_bases)?;
            temp_allocs.push((d_bb, c_bb));
            Self::launch_matmul_kernel(
                &self.inner.api,
                kernel,
                out_buf.raw_handle(),
                a.raw_handle(),
                b.raw_handle(),
                d_ab,
                d_bb,
                batches,
                m,
                n,
                k,
                label,
            )
        })();

        for (p, c) in temp_allocs {
            st.arena.release(&self.inner.api, p, c);
        }
        res?;
        self.inner.counters.on_vram_hit();
        if dtype == DType::F16 {
            self.inner.counters.on_f16_dispatch();
        } else if dtype == DType::BF16 {
            self.inner.counters.on_bf16_dispatch();
        }
        Ok(Some(out_buf))
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
        dst: Option<Arc<dyn DeviceVramBuffer>>,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        if !matches!(dtype, DType::F64 | DType::F32 | DType::F16 | DType::BF16) {
            return Ok(None);
        }
        let out_numel = m_total * n;
        if out_numel == 0 {
            return Ok(None);
        }
        let (d_bias, hb, bl) = match bias {
            None => (0u64, 0u32, 1u32),
            Some((vb, bshape)) => {
                let b_elems = bshape.iter().copied().product::<usize>().max(1);
                if b_elems == n || b_elems == 1 {
                    (vb.raw_handle(), 1u32, b_elems as u32)
                } else {
                    return Ok(None);
                }
            }
        };

        let out_bytes = dtype.storage_bytes(out_numel);
        let mut st = self.inner.state.lock();
        // SAFETY: Bind retained primary CUDA context.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent(fused_linear_act_vram)")?;

        let out_buf = self.resolve_or_alloc_vram(&mut st, out_bytes, dst)?;
        let kernel = match dtype {
            DType::F64 => st.kernels.fused_linear_act_f64,
            DType::F32 => st.kernels.fused_linear_act_f32,
            DType::F16 => st.kernels.fused_linear_act_f16,
            DType::BF16 => st.kernels.fused_linear_act_bf16,
            _ => unreachable!(),
        };
        let label = match dtype {
            DType::F64 => "mora_fused_linear_act_f64(vram)",
            DType::F32 => "mora_fused_linear_act_f32(vram)",
            DType::F16 => "mora_fused_linear_act_f16(vram)",
            DType::BF16 => "mora_fused_linear_act_bf16(vram)",
            _ => unreachable!(),
        };

        Self::launch_linear_act_kernel(
            &self.inner.api,
            kernel,
            out_buf.raw_handle(),
            x.raw_handle(),
            w.raw_handle(),
            d_bias,
            hb,
            bl,
            m_total,
            k,
            n,
            act,
            label,
        )?;
        self.inner.counters.on_vram_hit();
        if dtype == DType::F16 {
            self.inner.counters.on_f16_dispatch();
        } else if dtype == DType::BF16 {
            self.inner.counters.on_bf16_dispatch();
        }
        Ok(Some(out_buf))
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
        dst: Option<Arc<dyn DeviceVramBuffer>>,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        if !matches!(dtype, DType::F64 | DType::F32 | DType::F16 | DType::BF16)
            || m_total * n == 0
            || k == 0
        {
            return Ok(None);
        }
        let (gamma_buf, mut hg, mut gl) = match gamma {
            None => (None, 0u32, 1u32),
            Some((vg, gshape)) => {
                let g_elems = gshape.iter().copied().product::<usize>().max(1);
                if g_elems == k || g_elems == 1 {
                    (Some(vg), 1u32, g_elems as u32)
                } else {
                    return Ok(None);
                }
            }
        };
        let (bias_buf, mut hb, mut bl) = match bias {
            None => (None, 0u32, 1u32),
            Some((vb, bshape)) => {
                let b_elems = bshape.iter().copied().product::<usize>().max(1);
                if b_elems == n || b_elems == 1 {
                    (Some(vb), 1u32, b_elems as u32)
                } else {
                    return Ok(None);
                }
            }
        };

        let mut st = self.inner.state.lock();
        // SAFETY: Bind retained primary CUDA context.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent(rms_norm_linear_vram)")?;

        let out_numel = m_total * n;
        let out_buf = self.resolve_or_alloc_vram(&mut st, dtype.storage_bytes(out_numel), dst)?;
        let out_ptr = out_buf.raw_handle();
        let mut temp_allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(6);

        let res = (|| -> MoraResult<()> {
            let (mut d_inv, c_inv) = st.arena.alloc(&self.inner.api, m_total * 8)?;
            temp_allocs.push((d_inv, c_inv));
            let mut d_x =
                Self::vram_ptr_as_f64(&self.inner.api, &mut st, x, dtype, &mut temp_allocs)?;
            let mut d_w =
                Self::vram_ptr_as_f64(&self.inner.api, &mut st, w, dtype, &mut temp_allocs)?;
            let mut d_gamma = match gamma_buf {
                Some(vg) => {
                    Self::vram_ptr_as_f64(&self.inner.api, &mut st, vg, dtype, &mut temp_allocs)?
                }
                None => 0u64,
            };
            let mut d_bias = match bias_buf {
                Some(vb) => {
                    Self::vram_ptr_as_f64(&self.inner.api, &mut st, vb, dtype, &mut temp_allocs)?
                }
                None => 0u64,
            };
            let mut d_out_f64 = if dtype == DType::F64 {
                out_ptr
            } else {
                let (p, c) = st.arena.alloc(&self.inner.api, out_numel * 8)?;
                temp_allocs.push((p, c));
                p
            };
            let mut m_u = m_total as u32;
            let mut k_u = k as u32;
            let mut n_u = n as u32;
            let mut eps_v = eps;
            let mut ac = act_code(act);

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
                (self.inner.api.cu_launch_kernel)(
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
            cu_check(rc1, "cuLaunchKernel(mora_rms_inv_f64, vram)")?;

            let mut p2: [*mut c_void; 14] = [
                &raw mut d_out_f64 as *mut c_void,
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
                (self.inner.api.cu_launch_kernel)(
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
            cu_check(rc2, "cuLaunchKernel(mora_fused_rms_linear_act_f64, vram)")?;
            if dtype != DType::F64 {
                Self::launch_cast_from_f64(
                    &self.inner.api,
                    &st,
                    out_ptr,
                    d_out_f64,
                    out_numel,
                    dtype,
                )?;
            }
            // SAFETY: Synchronize default stream before returning resident VRAM buffer.
            let rc_sync = unsafe { (self.inner.api.cu_ctx_synchronize)() };
            cu_check(rc_sync, "cuCtxSynchronize(rms_norm_linear_vram)")
        })();

        for (p, c) in temp_allocs {
            st.arena.release(&self.inner.api, p, c);
        }
        res?;
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
        dst: Option<Arc<dyn DeviceVramBuffer>>,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        if !matches!(dtype, DType::F64 | DType::F32 | DType::F16 | DType::BF16)
            || q_shape.len() < 2
            || k_shape.len() < 2
            || v_shape.len() < 2
        {
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
                "CUDA FlashAttention shape mismatch: Q={q_shape:?}, K={k_shape:?}, V={v_shape:?}"
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

        let q_bases: Vec<u64> = (0..batches)
            .map(|b| (map_broadcast_batch(b, batch_out, batch_q) * s_q * d_k) as u64)
            .collect();
        let k_bases: Vec<u64> = (0..batches)
            .map(|b| (map_broadcast_batch(b, batch_out, batch_k) * s_k * d_k) as u64)
            .collect();
        let v_bases: Vec<u64> = (0..batches)
            .map(|b| (map_broadcast_batch(b, batch_out, batch_v) * s_k * d_v) as u64)
            .collect();

        let (mask_buf, mask_bases, mut mask_sq_s, mut mask_sk_s, mut has_mask_u) =
            if let Some((vm, mshape)) = mask {
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
                (Some(vm), mbases, sq_stride, sk_stride, 1u32)
            } else {
                (None, Vec::new(), 0u32, 0u32, 0u32)
            };

        let mut st = self.inner.state.lock();
        // SAFETY: Bind retained primary CUDA context.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent(flash_attn_vram)")?;

        let out_buf = self.resolve_or_alloc_vram(&mut st, dtype.storage_bytes(out_numel), dst)?;
        let out_ptr = out_buf.raw_handle();
        let mut temp_allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(9);
        let res = (|| -> MoraResult<()> {
            let mut dev_q =
                Self::vram_ptr_as_f64(&self.inner.api, &mut st, q, dtype, &mut temp_allocs)?;
            let mut dev_k =
                Self::vram_ptr_as_f64(&self.inner.api, &mut st, k, dtype, &mut temp_allocs)?;
            let mut dev_v =
                Self::vram_ptr_as_f64(&self.inner.api, &mut st, v, dtype, &mut temp_allocs)?;
            let mut dev_mask = match mask_buf {
                Some(vm) => {
                    Self::vram_ptr_as_f64(&self.inner.api, &mut st, vm, dtype, &mut temp_allocs)?
                }
                None => 0u64,
            };
            let mut dev_out = if dtype == DType::F64 {
                out_ptr
            } else {
                let (p, c) = st.arena.alloc(&self.inner.api, out_numel * 8)?;
                temp_allocs.push((p, c));
                p
            };
            let (mut dev_qb, c_qb) = Self::upload_slice(&self.inner.api, &mut st.arena, &q_bases)?;
            temp_allocs.push((dev_qb, c_qb));
            let (mut dev_kb, c_kb) = Self::upload_slice(&self.inner.api, &mut st.arena, &k_bases)?;
            temp_allocs.push((dev_kb, c_kb));
            let (mut dev_vb, c_vb) = Self::upload_slice(&self.inner.api, &mut st.arena, &v_bases)?;
            temp_allocs.push((dev_vb, c_vb));
            let mut dev_mb = if has_mask_u != 0 {
                let (pmb, cmb) = Self::upload_slice(&self.inner.api, &mut st.arena, &mask_bases)?;
                temp_allocs.push((pmb, cmb));
                pmb
            } else {
                0u64
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
                (self.inner.api.cu_launch_kernel)(
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
            cu_check(
                rc_fa,
                "cuLaunchKernel(mora_fused_flash_attention_f64, vram)",
            )?;
            if dtype != DType::F64 {
                Self::launch_cast_from_f64(
                    &self.inner.api,
                    &st,
                    out_ptr,
                    dev_out,
                    out_numel,
                    dtype,
                )?;
            }
            // SAFETY: Synchronize default stream before returning resident VRAM buffer.
            let rc_sync = unsafe { (self.inner.api.cu_ctx_synchronize)() };
            cu_check(rc_sync, "cuCtxSynchronize(flash_attn_vram)")
        })();

        for (p, c) in temp_allocs {
            st.arena.release(&self.inner.api, p, c);
        }
        res?;
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
        dst: Option<Arc<dyn DeviceVramBuffer>>,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        let total_slots = inputs.len() + steps.len();
        if !matches!(dtype, DType::F64 | DType::F32 | DType::F16 | DType::BF16)
            || out_numel == 0
            || steps.is_empty()
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
        let mut all_strides: Vec<u64> = Vec::with_capacity(input_shapes.len() * ndim);
        for s in input_shapes {
            all_strides.extend(compute_broadcast_strides(s, &eff_out_shape));
        }
        let (ops, arg_a, arg_b, imm) = Self::encode_ew_steps(steps);

        let mut st = self.inner.state.lock();
        // SAFETY: Bind retained primary CUDA context.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent(fused_ew_vram)")?;

        let out_buf = self.resolve_or_alloc_vram(&mut st, dtype.storage_bytes(out_numel), dst)?;
        let out_ptr = out_buf.raw_handle();
        let mut temp_allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(8 + inputs.len());
        let res = (|| -> MoraResult<()> {
            let mut dev_in_ptrs: Vec<u64> = Vec::with_capacity(inputs.len());
            for inp in inputs {
                let p =
                    Self::vram_ptr_as_f64(&self.inner.api, &mut st, *inp, dtype, &mut temp_allocs)?;
                dev_in_ptrs.push(p);
            }
            let mut d_out = if dtype == DType::F64 {
                out_ptr
            } else {
                let (p, c) = st.arena.alloc(&self.inner.api, out_numel * 8)?;
                temp_allocs.push((p, c));
                p
            };
            let (mut d_in_ptrs, c_ip) =
                Self::upload_slice(&self.inner.api, &mut st.arena, &dev_in_ptrs)?;
            temp_allocs.push((d_in_ptrs, c_ip));
            let (mut d_strides, c_st) =
                Self::upload_slice(&self.inner.api, &mut st.arena, &all_strides)?;
            temp_allocs.push((d_strides, c_st));
            let (mut d_oshape, c_os) =
                Self::upload_slice(&self.inner.api, &mut st.arena, &out_shape_u32)?;
            temp_allocs.push((d_oshape, c_os));
            let (mut d_ops, c_ops) = Self::upload_slice(&self.inner.api, &mut st.arena, &ops)?;
            temp_allocs.push((d_ops, c_ops));
            let (mut d_a, c_a) = Self::upload_slice(&self.inner.api, &mut st.arena, &arg_a)?;
            temp_allocs.push((d_a, c_a));
            let (mut d_b, c_b) = Self::upload_slice(&self.inner.api, &mut st.arena, &arg_b)?;
            temp_allocs.push((d_b, c_b));
            let (mut d_imm, c_imm) = Self::upload_slice(&self.inner.api, &mut st.arena, &imm)?;
            temp_allocs.push((d_imm, c_imm));

            let mut ndim_u = ndim as u32;
            let mut n_in_u = inputs.len() as u32;
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
                (self.inner.api.cu_launch_kernel)(
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
            cu_check(rc_ew, "cuLaunchKernel(mora_fused_ew_f64, vram)")?;
            if dtype != DType::F64 {
                Self::launch_cast_from_f64(&self.inner.api, &st, out_ptr, d_out, out_numel, dtype)?;
            }
            // SAFETY: Synchronize default stream before returning resident VRAM buffer.
            let rc_sync = unsafe { (self.inner.api.cu_ctx_synchronize)() };
            cu_check(rc_sync, "cuCtxSynchronize(fused_ew_vram)")
        })();

        for (p, c) in temp_allocs {
            st.arena.release(&self.inner.api, p, c);
        }
        res?;
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

        let mut st = self.inner.state.lock();
        // SAFETY: Bind this device's retained primary CUDA context to the calling thread.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent")?;

        let mut allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(5);
        let res = (|| -> MoraResult<()> {
            let (d_ab, c_ab) = Self::upload_slice(&self.inner.api, &mut st.arena, &a_bases)?;
            allocs.push((d_ab, c_ab));
            let (d_bb, c_bb) = Self::upload_slice(&self.inner.api, &mut st.arena, &b_bases)?;
            allocs.push((d_bb, c_bb));

            match dtype {
                DType::F16 | DType::BF16 => {
                    let is_bf16 = dtype == DType::BF16;
                    let a_u16: Vec<u16> = a
                        .iter()
                        .map(|&v| {
                            if is_bf16 {
                                f64_to_bf16_bits(v)
                            } else {
                                f64_to_f16_bits(v)
                            }
                        })
                        .collect();
                    let b_u16: Vec<u16> = b
                        .iter()
                        .map(|&v| {
                            if is_bf16 {
                                f64_to_bf16_bits(v)
                            } else {
                                f64_to_f16_bits(v)
                            }
                        })
                        .collect();
                    let (d_out, c_out) = st.arena.alloc(&self.inner.api, out_numel * 2)?;
                    allocs.push((d_out, c_out));
                    let (d_a, c_a) = Self::upload_slice(&self.inner.api, &mut st.arena, &a_u16)?;
                    allocs.push((d_a, c_a));
                    let (d_b, c_b) = Self::upload_slice(&self.inner.api, &mut st.arena, &b_u16)?;
                    allocs.push((d_b, c_b));
                    self.inner.counters.on_h2d();
                    self.inner.counters.on_h2d();
                    let (kernel, label) = if is_bf16 {
                        self.inner.counters.on_bf16_dispatch();
                        (st.kernels.matmul_bf16, "mora_matmul_bf16")
                    } else {
                        self.inner.counters.on_f16_dispatch();
                        (st.kernels.matmul_f16, "mora_matmul_f16")
                    };
                    Self::launch_matmul_kernel(
                        &self.inner.api,
                        kernel,
                        d_out,
                        d_a,
                        d_b,
                        d_ab,
                        d_bb,
                        batches,
                        m,
                        n,
                        k,
                        label,
                    )?;
                    let mut out_u16 = vec![0u16; out_numel];
                    Self::download_slice(&self.inner.api, d_out, &mut out_u16)?;
                    self.inner.counters.on_d2h();
                    for (dst, &bits) in out.iter_mut().zip(out_u16.iter()) {
                        *dst = if is_bf16 {
                            bf16_bits_to_f64(bits)
                        } else {
                            f16_bits_to_f64(bits)
                        };
                    }
                    Ok(())
                }
                _ => {
                    let (d_out, c_out) = st.arena.alloc(&self.inner.api, out_numel * 8)?;
                    allocs.push((d_out, c_out));
                    let (d_a, c_a) = Self::upload_slice(&self.inner.api, &mut st.arena, a)?;
                    allocs.push((d_a, c_a));
                    let (d_b, c_b) = Self::upload_slice(&self.inner.api, &mut st.arena, b)?;
                    allocs.push((d_b, c_b));
                    self.inner.counters.on_h2d();
                    self.inner.counters.on_h2d();
                    Self::launch_matmul_kernel(
                        &self.inner.api,
                        st.kernels.matmul_f64,
                        d_out,
                        d_a,
                        d_b,
                        d_ab,
                        d_bb,
                        batches,
                        m,
                        n,
                        k,
                        "mora_matmul_f64",
                    )?;
                    Self::download_slice(&self.inner.api, d_out, out)?;
                    self.inner.counters.on_d2h();
                    Ok(())
                }
            }
        })();

        for (p, c) in allocs {
            st.arena.release(&self.inner.api, p, c);
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

        let mut st = self.inner.state.lock();
        // SAFETY: Bind retained primary context.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent")?;

        let mut allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(4);
        let res = (|| -> MoraResult<()> {
            match dtype {
                DType::F16 | DType::BF16 => {
                    let is_bf16 = dtype == DType::BF16;
                    let enc = |v: f64| {
                        if is_bf16 {
                            f64_to_bf16_bits(v)
                        } else {
                            f64_to_f16_bits(v)
                        }
                    };
                    let x_u16: Vec<u16> = x.iter().map(|&v| enc(v)).collect();
                    let w_u16: Vec<u16> = w.iter().map(|&v| enc(v)).collect();
                    let (d_out, c_out) = st.arena.alloc(&self.inner.api, out_numel * 2)?;
                    allocs.push((d_out, c_out));
                    let (d_x, c_x) = Self::upload_slice(&self.inner.api, &mut st.arena, &x_u16)?;
                    allocs.push((d_x, c_x));
                    let (d_w, c_w) = Self::upload_slice(&self.inner.api, &mut st.arena, &w_u16)?;
                    allocs.push((d_w, c_w));
                    self.inner.counters.on_h2d();
                    self.inner.counters.on_h2d();
                    let (d_bias, hb, bl) = if !bias_slice.is_empty() {
                        let b_u16: Vec<u16> = bias_slice.iter().map(|&v| enc(v)).collect();
                        let (p, c) = Self::upload_slice(&self.inner.api, &mut st.arena, &b_u16)?;
                        allocs.push((p, c));
                        self.inner.counters.on_h2d();
                        (p, 1u32, b_u16.len() as u32)
                    } else {
                        (0u64, 0u32, 1u32)
                    };
                    let (kernel, label) = if is_bf16 {
                        self.inner.counters.on_bf16_dispatch();
                        (
                            st.kernels.fused_linear_act_bf16,
                            "mora_fused_linear_act_bf16",
                        )
                    } else {
                        self.inner.counters.on_f16_dispatch();
                        (st.kernels.fused_linear_act_f16, "mora_fused_linear_act_f16")
                    };
                    Self::launch_linear_act_kernel(
                        &self.inner.api,
                        kernel,
                        d_out,
                        d_x,
                        d_w,
                        d_bias,
                        hb,
                        bl,
                        m_total,
                        k,
                        n,
                        act,
                        label,
                    )?;
                    let mut out_u16 = vec![0u16; out_numel];
                    Self::download_slice(&self.inner.api, d_out, &mut out_u16)?;
                    self.inner.counters.on_d2h();
                    for (dst, &bits) in out.iter_mut().zip(out_u16.iter()) {
                        *dst = if is_bf16 {
                            bf16_bits_to_f64(bits)
                        } else {
                            f16_bits_to_f64(bits)
                        };
                    }
                    Ok(())
                }
                _ => {
                    let (d_out, c_out) = st.arena.alloc(&self.inner.api, out_numel * 8)?;
                    allocs.push((d_out, c_out));
                    let (d_x, c_x) = Self::upload_slice(&self.inner.api, &mut st.arena, x)?;
                    allocs.push((d_x, c_x));
                    let (d_w, c_w) = Self::upload_slice(&self.inner.api, &mut st.arena, w)?;
                    allocs.push((d_w, c_w));
                    self.inner.counters.on_h2d();
                    self.inner.counters.on_h2d();
                    let (d_bias, hb, bl) = if !bias_slice.is_empty() {
                        let (p, c) =
                            Self::upload_slice(&self.inner.api, &mut st.arena, bias_slice)?;
                        allocs.push((p, c));
                        self.inner.counters.on_h2d();
                        (p, 1u32, bias_slice.len() as u32)
                    } else {
                        (0u64, 0u32, 1u32)
                    };
                    Self::launch_linear_act_kernel(
                        &self.inner.api,
                        st.kernels.fused_linear_act_f64,
                        d_out,
                        d_x,
                        d_w,
                        d_bias,
                        hb,
                        bl,
                        m_total,
                        k,
                        n,
                        act,
                        "mora_fused_linear_act_f64",
                    )?;
                    Self::download_slice(&self.inner.api, d_out, out)?;
                    self.inner.counters.on_d2h();
                    Ok(())
                }
            }
        })();

        for (p, c) in allocs {
            st.arena.release(&self.inner.api, p, c);
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

        let mut st = self.inner.state.lock();
        // SAFETY: Bind retained primary CUDA context.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent")?;

        let mut allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(6);
        let res = (|| -> MoraResult<()> {
            let (mut d_out, c_out) = st.arena.alloc(&self.inner.api, out_numel * 8)?;
            allocs.push((d_out, c_out));
            let (mut d_inv, c_inv) = st.arena.alloc(&self.inner.api, m_total * 8)?;
            allocs.push((d_inv, c_inv));
            let (mut d_x, c_x) = Self::upload_slice(&self.inner.api, &mut st.arena, x)?;
            allocs.push((d_x, c_x));
            let (mut d_w, c_w) = Self::upload_slice(&self.inner.api, &mut st.arena, w)?;
            allocs.push((d_w, c_w));
            self.inner.counters.on_h2d();
            self.inner.counters.on_h2d();
            let (mut d_gamma, mut hg, mut gl) = if !gamma_slice.is_empty() {
                let (p, c) = Self::upload_slice(&self.inner.api, &mut st.arena, gamma_slice)?;
                allocs.push((p, c));
                self.inner.counters.on_h2d();
                (p, 1u32, gamma_slice.len() as u32)
            } else {
                (0u64, 0u32, 1u32)
            };
            let (mut d_bias, mut hb, mut bl) = if !bias_slice.is_empty() {
                let (p, c) = Self::upload_slice(&self.inner.api, &mut st.arena, bias_slice)?;
                allocs.push((p, c));
                self.inner.counters.on_h2d();
                (p, 1u32, bias_slice.len() as u32)
            } else {
                (0u64, 0u32, 1u32)
            };

            let mut m_u = m_total as u32;
            let mut k_u = k as u32;
            let mut n_u = n as u32;
            let mut eps_v = eps;
            let mut ac = act_code(act);

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
                (self.inner.api.cu_launch_kernel)(
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
                (self.inner.api.cu_launch_kernel)(
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
            let rc_sync = unsafe { (self.inner.api.cu_ctx_synchronize)() };
            cu_check(rc_sync, "cuCtxSynchronize(mora_fused_rms_linear_act_f64)")?;
            Self::download_slice(&self.inner.api, d_out, out)?;
            self.inner.counters.on_d2h();
            Ok(())
        })();

        for (p, c) in allocs {
            st.arena.release(&self.inner.api, p, c);
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

        let mut st = self.inner.state.lock();
        // SAFETY: Bind retained primary CUDA context.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent")?;

        let mut allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(9);
        let res = (|| -> MoraResult<()> {
            let (mut dev_out, c_out) = st.arena.alloc(&self.inner.api, out_numel * 8)?;
            allocs.push((dev_out, c_out));
            let (mut dev_q, c_q) = Self::upload_slice(&self.inner.api, &mut st.arena, q)?;
            allocs.push((dev_q, c_q));
            let (mut dev_k, c_k) = Self::upload_slice(&self.inner.api, &mut st.arena, k)?;
            allocs.push((dev_k, c_k));
            let (mut dev_v, c_v) = Self::upload_slice(&self.inner.api, &mut st.arena, v)?;
            allocs.push((dev_v, c_v));
            self.inner.counters.on_h2d();
            self.inner.counters.on_h2d();
            self.inner.counters.on_h2d();
            let (mut dev_qb, c_qb) = Self::upload_slice(&self.inner.api, &mut st.arena, &q_bases)?;
            allocs.push((dev_qb, c_qb));
            let (mut dev_kb, c_kb) = Self::upload_slice(&self.inner.api, &mut st.arena, &k_bases)?;
            allocs.push((dev_kb, c_kb));
            let (mut dev_vb, c_vb) = Self::upload_slice(&self.inner.api, &mut st.arena, &v_bases)?;
            allocs.push((dev_vb, c_vb));
            let (mut dev_mask, mut dev_mb) = if has_mask_u != 0 {
                let (pm, cm) = Self::upload_slice(&self.inner.api, &mut st.arena, mask_vals)?;
                allocs.push((pm, cm));
                self.inner.counters.on_h2d();
                let (pmb, cmb) = Self::upload_slice(&self.inner.api, &mut st.arena, &mask_bases)?;
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
                (self.inner.api.cu_launch_kernel)(
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
            let rc_sync = unsafe { (self.inner.api.cu_ctx_synchronize)() };
            cu_check(rc_sync, "cuCtxSynchronize(mora_fused_flash_attention_f64)")?;
            Self::download_slice(&self.inner.api, dev_out, out)?;
            self.inner.counters.on_d2h();
            Ok(())
        })();

        for (p, c) in allocs {
            st.arena.release(&self.inner.api, p, c);
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

        let (ops, arg_a, arg_b, imm) = Self::encode_ew_steps(steps);

        let mut st = self.inner.state.lock();
        // SAFETY: Bind retained primary CUDA context.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent")?;

        let mut allocs: Vec<(CuDevicePtr, usize)> = Vec::with_capacity(input_flats.len() + 8);
        let res = (|| -> MoraResult<()> {
            let (mut d_out, c_out) = st.arena.alloc(&self.inner.api, out_numel * 8)?;
            allocs.push((d_out, c_out));

            let mut dev_in_ptrs = Vec::with_capacity(input_flats.len());
            for flat in input_flats {
                let (p, c) = Self::upload_slice(&self.inner.api, &mut st.arena, flat)?;
                allocs.push((p, c));
                self.inner.counters.on_h2d();
                dev_in_ptrs.push(p);
            }
            let (mut d_in_ptrs, c_ip) =
                Self::upload_slice(&self.inner.api, &mut st.arena, &dev_in_ptrs)?;
            allocs.push((d_in_ptrs, c_ip));
            let (mut d_strides, c_st) =
                Self::upload_slice(&self.inner.api, &mut st.arena, &all_strides)?;
            allocs.push((d_strides, c_st));
            let (mut d_oshape, c_os) =
                Self::upload_slice(&self.inner.api, &mut st.arena, &out_shape_u32)?;
            allocs.push((d_oshape, c_os));
            let (mut d_ops, c_ops) = Self::upload_slice(&self.inner.api, &mut st.arena, &ops)?;
            allocs.push((d_ops, c_ops));
            let (mut d_a, c_a) = Self::upload_slice(&self.inner.api, &mut st.arena, &arg_a)?;
            allocs.push((d_a, c_a));
            let (mut d_b, c_b) = Self::upload_slice(&self.inner.api, &mut st.arena, &arg_b)?;
            allocs.push((d_b, c_b));
            let (mut d_imm, c_imm) = Self::upload_slice(&self.inner.api, &mut st.arena, &imm)?;
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
                (self.inner.api.cu_launch_kernel)(
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
            let rc_sync = unsafe { (self.inner.api.cu_ctx_synchronize)() };
            cu_check(rc_sync, "cuCtxSynchronize(mora_fused_ew_f64)")?;
            Self::download_slice(&self.inner.api, d_out, out)?;
            self.inner.counters.on_d2h();
            Ok(())
        })();

        for (p, c) in allocs {
            st.arena.release(&self.inner.api, p, c);
        }
        res?;
        quantize_slice_in_place(out, dtype);
        Ok(())
    }

    fn launch_custom_kernel(
        &self,
        kernel: &crate::common::kernel_ir::KernelProgram,
        grid: [u32; 3],
        block: [u32; 3],
        args: &[crate::runtime::gpu::kernel_compiler::KernelLaunchArg],
    ) -> MoraResult<bool> {
        use crate::runtime::gpu::kernel_compiler::{KernelLaunchArg, emit_ptx_for_block};

        let mut st = self.inner.state.lock();
        // SAFETY: Bind retained primary CUDA context before compiling/launching custom kernel.
        let rc_ctx = unsafe { (self.inner.api.cu_ctx_set_current)(st.ctx) };
        cu_check(rc_ctx, "cuCtxSetCurrent(launch_custom_kernel)")?;

        let local_size = [block[0].max(1), block[1].max(1), block[2].max(1)];
        let block_key = if kernel.shared.is_empty() {
            [0, 0, 0]
        } else {
            local_size
        };
        let cache_key = (kernel.content_hash(), block_key);
        let func = if let Some(&(_, f)) = st.custom_modules.get(&cache_key) {
            f
        } else {
            let ptx = emit_ptx_for_block(
                kernel,
                if kernel.shared.is_empty() {
                    None
                } else {
                    Some(local_size)
                },
            );
            let ptx_nul = CString::new(ptx).map_err(|e| {
                MoraError::Runtime(format!("CUDA custom kernel PTX contains interior NUL: {e}"))
            })?;
            let mut module: CuModule = ptr::null_mut();
            let rc = if let Some(load_ex) = self.inner.api.cu_module_load_data_ex {
                let mut err_buf = vec![0u8; 16384];
                let mut opts: [c_uint; 2] =
                    [CU_JIT_ERROR_LOG_BUFFER, CU_JIT_ERROR_LOG_BUFFER_SIZE_BYTES];
                let mut vals: [*mut c_void; 2] = [
                    err_buf.as_mut_ptr() as *mut c_void,
                    err_buf.len() as *mut c_void,
                ];
                // SAFETY: `ptx_nul` is NUL-terminated; `opts`/`vals` match CUDA JIT error log ABI.
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
                        "CUDA JIT failed for kernel '{}': CUresult({rc})\n{log}",
                        kernel.name
                    )));
                }
                rc
            } else {
                // SAFETY: `ptx_nul` is a valid NUL-terminated PTX source buffer.
                unsafe {
                    (self.inner.api.cu_module_load_data)(
                        &mut module,
                        ptx_nul.as_ptr() as *const c_void,
                    )
                }
            };
            cu_check(rc, "cuModuleLoadData(custom_kernel)")?;
            let f = match load_kernel_fn(&self.inner.api, module, &kernel.name) {
                Ok(f) => f,
                Err(e) => {
                    // SAFETY: Unload newly loaded module on symbol lookup failure.
                    unsafe {
                        let _ = (self.inner.api.cu_module_unload)(module);
                    }
                    return Err(e);
                }
            };
            st.custom_modules.insert(cache_key, (module, f));
            f
        };

        // Keep concrete storage slots pinned in a Vec while building raw `*mut c_void` param pointers.
        enum ParamSlot {
            Ptr(CuDevicePtr),
            F32(f32),
            F64(f64),
            U32(u32),
        }
        let mut slots: Vec<ParamSlot> = Vec::with_capacity(args.len());
        for a in args {
            match *a {
                KernelLaunchArg::VramBuffer { handle, .. } => slots.push(ParamSlot::Ptr(handle)),
                KernelLaunchArg::F32(v) => slots.push(ParamSlot::F32(v)),
                KernelLaunchArg::F64(v) => slots.push(ParamSlot::F64(v)),
                KernelLaunchArg::I32(v) => slots.push(ParamSlot::U32(v as u32)),
            }
        }
        let mut param_ptrs: Vec<*mut c_void> = Vec::with_capacity(slots.len());
        for slot in &mut slots {
            let p = match slot {
                ParamSlot::Ptr(v) => &raw mut *v as *mut c_void,
                ParamSlot::F32(v) => &raw mut *v as *mut c_void,
                ParamSlot::F64(v) => &raw mut *v as *mut c_void,
                ParamSlot::U32(v) => &raw mut *v as *mut c_void,
            };
            param_ptrs.push(p);
        }

        // SAFETY: `func` is a live `CUfunction` in the bound context and `param_ptrs` points to
        // live `slots` matching the exact `.param` order emitted by `emit_ptx(kernel)`.
        let rc_launch = unsafe {
            (self.inner.api.cu_launch_kernel)(
                func,
                grid[0].max(1),
                grid[1].max(1),
                grid[2].max(1),
                block[0].max(1),
                block[1].max(1),
                block[2].max(1),
                0,
                ptr::null_mut(),
                param_ptrs.as_mut_ptr(),
                ptr::null_mut(),
            )
        };
        cu_check(rc_launch, "cuLaunchKernel(custom_kernel)")?;
        // SAFETY: Synchronize default stream so subsequent VRAM reads/D2H observe completed writes.
        let rc_sync = unsafe { (self.inner.api.cu_ctx_synchronize)() };
        cu_check(rc_sync, "cuCtxSynchronize(custom_kernel)")?;
        self.inner.counters.on_vram_hit();
        Ok(true)
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

        let inner = Arc::new(CudaDeviceInner {
            device,
            cu_dev,
            api: Arc::clone(&api),
            state: Mutex::new(CudaContextState {
                ctx,
                kernels,
                arena: CudaVramArena::new(),
                custom_modules: HashMap::new(),
            }),
            counters: VramCounters::default(),
        });
        let backend = Arc::new(CudaDeviceBackend {
            inner,
            info: info.clone(),
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
