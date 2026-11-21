//! v0.110.0: Native Apple Metal Framework (`Metal.framework`) Hardware Backend (macOS).
//!
//! Architecture:
//! - Zero compile-time SDK / Cargo dependencies: on `#[cfg(target_os = "macos")]`, dynamically loads
//!   `/System/Library/Frameworks/Metal.framework/Metal` and `/usr/lib/libobjc.A.dylib`, obtains the
//!   default `MTLDevice` via `MTLCreateSystemDefaultDevice()`, compiles Mora's MSL (Metal Shading
//!   Language) compute shader at runtime via `newLibraryWithSource:options:error:`, and dispatches
//!   fused `Linear + Bias + Activation` & `MatMul` on native Apple Silicon / AMD Metal GPUs.
//! - On non-macOS hosts (Windows/Linux), cleanly returns empty hardware list so `metal:N` tagged
//!   graphs transparently fall back to `EmulatedDeviceBackend`.

use crate::runtime::gpu::HardwareBackendEntry;

#[cfg(not(target_os = "macos"))]
pub(crate) fn probe_metal_backends() -> Vec<HardwareBackendEntry> {
    Vec::new()
}

#[cfg(target_os = "macos")]
mod macos_impl {
    use std::ffi::{CStr, c_char, c_void};
    use std::ptr;
    use std::sync::Arc;

    use crate::common::sync::Mutex;

    use crate::common::{DType, Device};
    use crate::error::{MoraError, MoraResult};
    use crate::runtime::compute_dag::{DeviceBackend, FusedAct, FusedEwStep};
    use crate::runtime::gpu::{DynLib, GpuDeviceInfo, HardwareBackendEntry};
    use crate::runtime::simd_kernels;

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

    const MORA_METAL_MSL: &str = r#"
#include <metal_stdlib>
using namespace metal;

kernel void mora_fused_linear_f32(
    device const float* A [[buffer(0)]],
    device const float* B [[buffer(1)]],
    device const float* Bias [[buffer(2)]],
    device float* Out [[buffer(3)]],
    device const uint* Params [[buffer(4)]],
    uint2 gid [[thread_position_in_grid]])
{
    uint M = Params[0];
    uint N = Params[1];
    uint K = Params[2];
    uint has_bias = Params[3];
    uint bias_len = Params[4];
    uint act = Params[5];
    uint col = gid.x;
    uint row = gid.y;
    if (row >= M || col >= N) return;
    float acc = 0.0f;
    uint row_k = row * K;
    for (uint t = 0; t < K; ++t) {
        acc = fma(A[row_k + t], B[t * N + col], acc);
    }
    if (has_bias != 0) {
        acc += Bias[col % bias_len];
    }
    if (act == 1) {
        acc = max(acc, 0.0f);
    } else if (act == 2) {
        float c = 0.79788456f * (acc + 0.044715f * acc * acc * acc);
        acc = 0.5f * acc * (1.0f + tanh(c));
    } else if (act == 3) {
        acc = acc / (1.0f + exp(clamp(-acc, -40.0f, 40.0f)));
    } else if (act == 4) {
        acc = tanh(acc);
    } else if (act == 5) {
        acc = 1.0f / (1.0f + exp(clamp(-acc, -40.0f, 40.0f)));
    }
    Out[row * N + col] = acc;
}
"#;

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
        pipeline: Id,
    }

    pub(crate) struct MetalDeviceBackend {
        device: Device,
        objc: Arc<ObjC>,
        info: GpuDeviceInfo,
        state: Mutex<MetalState>,
    }

    // SAFETY: All Metal device/queue/pipeline ObjC messages are serialized under `self.state.lock()`.
    unsafe impl Send for MetalDeviceBackend {}
    // SAFETY: Serialized access via `Mutex<MetalState>`.
    unsafe impl Sync for MetalDeviceBackend {}

    impl Drop for MetalDeviceBackend {
        fn drop(&mut self) {
            let st = self.state.lock();
            self.objc.release(st.pipeline);
            self.objc.release(st.queue);
            self.objc.release(st.mtl_dev);
        }
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

    impl MetalDeviceBackend {
        #[allow(clippy::too_many_arguments)]
        fn dispatch_linear_f32(
            &self,
            a: &[f32],
            b: &[f32],
            bias: &[f32],
            m: usize,
            n: usize,
            k: usize,
            act: u32,
            out_f32: &mut [f32],
        ) -> MoraResult<()> {
            let st = self.state.lock();
            let has_bias = if bias.is_empty() { 0u32 } else { 1u32 };
            let bias_len = bias.len().max(1) as u32;
            let dummy = [0.0f32];
            let eff_bias = if bias.is_empty() { &dummy[..] } else { bias };
            let params: [u32; 6] = [m as u32, n as u32, k as u32, has_bias, bias_len, act];

            // SAFETY: Allocates shared `MTLBuffer`s, encodes compute dispatch, waits, and releases.
            unsafe {
                let s_new_buf = (self.objc.sel)(
                    b"newBufferWithBytes:length:options:\0".as_ptr() as *const c_char
                );
                let s_new_len =
                    (self.objc.sel)(b"newBufferWithLength:options:\0".as_ptr() as *const c_char);
                let s_cmd_buf = (self.objc.sel)(b"commandBuffer\0".as_ptr() as *const c_char);
                let s_enc = (self.objc.sel)(b"computeCommandEncoder\0".as_ptr() as *const c_char);
                let s_set_pipe =
                    (self.objc.sel)(b"setComputePipelineState:\0".as_ptr() as *const c_char);
                let s_set_buf =
                    (self.objc.sel)(b"setBuffer:offset:atIndex:\0".as_ptr() as *const c_char);
                let s_dispatch = (self.objc.sel)(
                    b"dispatchThreads:threadsPerThreadgroup:\0".as_ptr() as *const c_char,
                );
                let s_end = (self.objc.sel)(b"endEncoding\0".as_ptr() as *const c_char);
                let s_commit = (self.objc.sel)(b"commit\0".as_ptr() as *const c_char);
                let s_wait = (self.objc.sel)(b"waitUntilCompleted\0".as_ptr() as *const c_char);
                let s_contents = (self.objc.sel)(b"contents\0".as_ptr() as *const c_char);

                let ba = (self.objc.msg_new_buf)(
                    st.mtl_dev,
                    s_new_buf,
                    a.as_ptr() as *const c_void,
                    std::mem::size_of_val(a).max(4),
                    0,
                );
                let bb = (self.objc.msg_new_buf)(
                    st.mtl_dev,
                    s_new_buf,
                    b.as_ptr() as *const c_void,
                    std::mem::size_of_val(b).max(4),
                    0,
                );
                let bbias = (self.objc.msg_new_buf)(
                    st.mtl_dev,
                    s_new_buf,
                    eff_bias.as_ptr() as *const c_void,
                    std::mem::size_of_val(eff_bias).max(4),
                    0,
                );
                let bout = (self.objc.msg_new_buf_len)(
                    st.mtl_dev,
                    s_new_len,
                    std::mem::size_of_val(out_f32).max(4),
                    0,
                );
                let bparams = (self.objc.msg_new_buf)(
                    st.mtl_dev,
                    s_new_buf,
                    params.as_ptr() as *const c_void,
                    std::mem::size_of_val(&params),
                    0,
                );

                if ba.is_null()
                    || bb.is_null()
                    || bbias.is_null()
                    || bout.is_null()
                    || bparams.is_null()
                {
                    self.objc.release(ba);
                    self.objc.release(bb);
                    self.objc.release(bbias);
                    self.objc.release(bout);
                    self.objc.release(bparams);
                    return Err(MoraError::Runtime(
                        "Metal buffer allocation failed".to_string(),
                    ));
                }

                let cmd = (self.objc.msg0)(st.queue, s_cmd_buf);
                let enc = (self.objc.msg0)(cmd, s_enc);
                (self.objc.msg_set_pipe)(enc, s_set_pipe, st.pipeline);
                (self.objc.msg_set_buf)(enc, s_set_buf, ba, 0, 0);
                (self.objc.msg_set_buf)(enc, s_set_buf, bb, 0, 1);
                (self.objc.msg_set_buf)(enc, s_set_buf, bbias, 0, 2);
                (self.objc.msg_set_buf)(enc, s_set_buf, bout, 0, 3);
                (self.objc.msg_set_buf)(enc, s_set_buf, bparams, 0, 4);
                let grid = MtlSize {
                    width: n.max(1),
                    height: m.max(1),
                    depth: 1,
                };
                let tg = MtlSize {
                    width: n.clamp(1, 16),
                    height: m.clamp(1, 16),
                    depth: 1,
                };
                (self.objc.msg_dispatch)(enc, s_dispatch, grid, tg);
                (self.objc.msg_void)(enc, s_end);
                (self.objc.msg_void)(cmd, s_commit);
                (self.objc.msg_void)(cmd, s_wait);

                let ptr_out = (self.objc.msg0)(bout, s_contents) as *const f32;
                if !ptr_out.is_null() {
                    ptr::copy_nonoverlapping(ptr_out, out_f32.as_mut_ptr(), out_f32.len());
                }
                self.objc.release(ba);
                self.objc.release(bb);
                self.objc.release(bbias);
                self.objc.release(bout);
                self.objc.release(bparams);
            }
            Ok(())
        }
    }

    impl DeviceBackend for MetalDeviceBackend {
        fn kind(&self) -> Device {
            self.device.clone()
        }

        fn name(&self) -> &str {
            "metal"
        }

        fn simd_isa(&self) -> &'static str {
            "msl-f32"
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
            if a_shape.len() != 2 || b_shape.len() != 2 {
                simd_kernels::matmul_simd_into(a, b, a_shape, b_shape, out)?;
                for v in out.iter_mut() {
                    *v = dtype.quantize_f64(*v);
                }
                return Ok(());
            }
            let (m, k, n) = (a_shape[0], a_shape[1], b_shape[1]);
            out.resize(m * n, 0.0);
            if m * n == 0 || k == 0 {
                out.fill(0.0);
                return Ok(());
            }
            let a32: Vec<f32> = a.iter().map(|&v| v as f32).collect();
            let b32: Vec<f32> = b.iter().map(|&v| v as f32).collect();
            let mut o32 = vec![0.0f32; m * n];
            self.dispatch_linear_f32(&a32, &b32, &[], m, n, k, 0, &mut o32)?;
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
            let bias32 = match bias {
                None => Vec::new(),
                Some((bv, _)) if bv.len() == n || bv.len() == 1 => {
                    bv.iter().map(|&v| v as f32).collect()
                }
                Some(_) => {
                    simd_kernels::fused_linear_act_into(
                        x, w, bias, act, m_total, k, n, out_shape, dtype, out,
                    );
                    return Ok(());
                }
            };
            let x32: Vec<f32> = x.iter().map(|&v| v as f32).collect();
            let w32: Vec<f32> = w.iter().map(|&v| v as f32).collect();
            let mut o32 = vec![0.0f32; m_total * n];
            self.dispatch_linear_f32(&x32, &w32, &bias32, m_total, n, k, act_code(act), &mut o32)?;
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
            let gamma_slice = match gamma {
                None => None,
                Some((gv, _)) if gv.len() == k || gv.len() == 1 => Some(gv),
                Some(_) => {
                    simd_kernels::fused_rms_norm_linear_into(
                        x, eps, gamma, w, bias, act, m_total, k, n, x_shape, out_shape, dtype, out,
                    );
                    return Ok(());
                }
            };
            let mut x_norm = vec![0.0f64; m_total * k];
            for row in 0..m_total {
                let src = &x[row * k..(row + 1) * k];
                let dst = &mut x_norm[row * k..(row + 1) * k];
                let sq: f64 = src.iter().map(|&v| v * v).sum();
                let inv = 1.0 / (sq / (k as f64) + eps).sqrt();
                for j in 0..k {
                    let g = gamma_slice.map_or(1.0, |gv| gv[j % gv.len()]);
                    dst[j] = src[j] * inv * g;
                }
            }
            self.run_fused_linear_act_into(
                &x_norm, w, bias, act, m_total, k, n, out_shape, dtype, out,
            )
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
            simd_kernels::fused_flash_attention_into(
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
            )
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
            simd_kernels::fused_elementwise_into(
                input_flats,
                input_shapes,
                steps,
                out_shape,
                out_numel,
                dtype,
                out,
            )
        }
    }

    pub(super) fn probe() -> Vec<HardwareBackendEntry> {
        let Some(objc) = ObjC::load() else {
            return Vec::new();
        };
        // SAFETY: Obtains the default Metal device and compiles Mora's MSL shader at runtime.
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
            let s_new_fn = (objc.sel)(b"newFunctionWithName:\0".as_ptr() as *const c_char);
            let s_new_pipe = (objc.sel)(
                b"newComputePipelineStateWithFunction:error:\0".as_ptr() as *const c_char
            );

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
            let lib = (objc.msg_new_lib)(mtl_dev, s_new_lib, ns_src, ptr::null_mut(), &mut err);
            objc.release(ns_src);
            if queue.is_null() || lib.is_null() {
                objc.release(lib);
                objc.release(queue);
                objc.release(mtl_dev);
                return Vec::new();
            }
            let ns_fn = objc.make_nsstring("mora_fused_linear_f32");
            let func = (objc.msg_new_fn)(lib, s_new_fn, ns_fn);
            objc.release(ns_fn);
            let mut pipe_err: Id = ptr::null_mut();
            let pipeline = if !func.is_null() {
                (objc.msg_new_pipe)(mtl_dev, s_new_pipe, func, &mut pipe_err)
            } else {
                ptr::null_mut()
            };
            objc.release(func);
            objc.release(lib);
            if pipeline.is_null() {
                objc.release(queue);
                objc.release(mtl_dev);
                return Vec::new();
            }

            let device = Device::Metal(0);
            let info = GpuDeviceInfo {
                device: device.clone(),
                driver: "metal".to_string(),
                name: cname,
                total_vram_bytes: vram,
                compute_capability: "metal-msl".to_string(),
                multiprocessors: 1,
                hardware_accelerated: true,
                driver_version: "Metal.framework".to_string(),
            };
            let backend = Arc::new(MetalDeviceBackend {
                device,
                objc: Arc::clone(&objc),
                info: info.clone(),
                state: Mutex::new(MetalState {
                    mtl_dev,
                    queue,
                    pipeline,
                }),
            });
            vec![HardwareBackendEntry { info, backend }]
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn probe_metal_backends() -> Vec<HardwareBackendEntry> {
    macos_impl::probe()
}
