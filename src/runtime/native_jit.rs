//! P4-3: Zero-dependency strict W^X (`RW -> RX`) native x86-64 SSE2/AVX2 machine-code kernel
//! emitter for [`crate::runtime::compute_dag::TensorComputeDag`] `FusedElementwise` chains.
//!
//! Security & Architecture Invariants (`AGENTS.md §0`):
//! - **Zero external Cargo dependencies**: allocates OS pages directly via `VirtualAlloc` /
//!   `VirtualProtect` / `VirtualFree` / `FlushInstructionCache` (Windows) or `mmap` /
//!   `mprotect` / `munmap` (Unix).
//! - **Strict W^X**: pages are allocated strictly `RW` (`PAGE_READWRITE` / `PROT_READ|PROT_WRITE`),
//!   populated with emitted x86-64 machine code, and flipped strictly to `RX`
//!   (`PAGE_EXECUTE_READ` / `PROT_READ|PROT_EXEC`). Pages are NEVER mapped `RWX`.
//! - **Cross-ABI compliance**: preserves all callee-saved registers across both Windows x64 and
//!   System V AMD64 ABIs by normalizing arguments into caller-saved GP registers (`r10`, `r11`,
//!   `r8`, `r9`) and using only caller-saved `xmm0..xmm5`/`ymm0..ymm5` plus a 32-byte aligned
//!   stack scratch array.
//! - **Kill switch & portability**: setting `MORA_DISABLE_NATIVE_JIT=1` disables native emission
//!   and falls back cleanly to `simd_kernels::fused_elementwise_into`; non-x86-64 targets always
//!   fall back cleanly with zero `unsafe` code compiled.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::common::DType;
use crate::error::MoraResult;
use crate::runtime::compute_dag::{FusedAct, FusedEwStep};

/// Live runtime telemetry for the native W^X x86-64 elementwise & GEMM JIT backend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct NativeJitStats {
    pub(crate) isa: &'static str,
    pub(crate) enabled: bool,
    pub(crate) compiled_kernels: u64,
    pub(crate) cache_hits: u64,
    pub(crate) native_dispatches: u64,
    pub(crate) f32_native_dispatches: u64,
    pub(crate) gemm_native_dispatches: u64,
    pub(crate) fallback_dispatches: u64,
}

static STAT_COMPILED_KERNELS: AtomicU64 = AtomicU64::new(0);
static STAT_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static STAT_NATIVE_DISPATCHES: AtomicU64 = AtomicU64::new(0);
static STAT_F32_NATIVE_DISPATCHES: AtomicU64 = AtomicU64::new(0);
static STAT_GEMM_NATIVE_DISPATCHES: AtomicU64 = AtomicU64::new(0);
static STAT_FALLBACK_DISPATCHES: AtomicU64 = AtomicU64::new(0);

/// Maximum total virtual slots (`n_inputs + steps.len()`) kept in the 32B-aligned stack scratchpad.
const MAX_JIT_SLOTS: usize = 64;

/// Return whether the native x86-64 W^X JIT is active (supported CPU + kill switch unset).
pub(crate) fn is_enabled() -> bool {
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
    #[cfg(target_arch = "x86_64")]
    {
        !matches!(
            std::env::var("MORA_DISABLE_NATIVE_JIT").ok().as_deref(),
            Some("1" | "true" | "TRUE" | "yes" | "YES")
        )
    }
}

/// Active native JIT target ISA name (`"x86_64-avx2"`, `"x86_64-sse2"`, or `"fallback"`).
pub(crate) fn active_isa() -> &'static str {
    #[cfg(target_arch = "x86_64")]
    {
        if !is_enabled() {
            return "disabled";
        }
        if std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("avx")
            && std::arch::is_x86_feature_detected!("fma")
        {
            "x86_64-avx2-fma"
        } else if std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("avx")
        {
            "x86_64-avx2"
        } else {
            "x86_64-sse2"
        }
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        "fallback"
    }
}

/// Snapshot native JIT telemetry counters.
pub(crate) fn stats() -> NativeJitStats {
    NativeJitStats {
        isa: active_isa(),
        enabled: is_enabled(),
        compiled_kernels: STAT_COMPILED_KERNELS.load(Ordering::Relaxed),
        cache_hits: STAT_CACHE_HITS.load(Ordering::Relaxed),
        native_dispatches: STAT_NATIVE_DISPATCHES.load(Ordering::Relaxed),
        f32_native_dispatches: STAT_F32_NATIVE_DISPATCHES.load(Ordering::Relaxed),
        gemm_native_dispatches: STAT_GEMM_NATIVE_DISPATCHES.load(Ordering::Relaxed),
        fallback_dispatches: STAT_FALLBACK_DISPATCHES.load(Ordering::Relaxed),
    }
}

/// Check whether a single `FusedEwStep` is supported bit-exactly by the x86-64 SSE2/AVX2 emitter.
pub(crate) fn is_step_supported(step: &FusedEwStep) -> bool {
    match *step {
        FusedEwStep::Add(_, _)
        | FusedEwStep::Sub(_, _)
        | FusedEwStep::Mul(_, _)
        | FusedEwStep::AddScalar(_, _)
        | FusedEwStep::SubScalar(_, _, _)
        | FusedEwStep::MulScalar(_, _)
        | FusedEwStep::Neg(_)
        | FusedEwStep::Abs(_)
        | FusedEwStep::Sqrt(_)
        | FusedEwStep::Relu(_)
        | FusedEwStep::StepPos(_)
        | FusedEwStep::Sign(_) => true,
        // `x / c` with non-zero finite/infinite `c` never triggers division-by-zero error;
        // `Div(a, b)` and `c / a` fall back to `simd_kernels` to preserve exact div-by-zero errors.
        FusedEwStep::DivScalar(_, c, false) => c != 0.0,
        _ => false,
    }
}

/// Check whether a `FusedElementwise` step sequence is structurally eligible for native JIT compilation.
pub(crate) fn is_chain_supported(n_inputs: usize, steps: &[FusedEwStep]) -> bool {
    n_inputs > 0
        && !steps.is_empty()
        && n_inputs + steps.len() <= MAX_JIT_SLOTS
        && steps.iter().all(is_step_supported)
}

/// Attempt to execute a `FusedElementwise` chain via strict W^X x86-64 native machine code.
/// Returns `Ok(true)` if executed natively (writing quantized results into `out`), or `Ok(false)`
/// if the caller should fall back to `simd_kernels::fused_elementwise_into`.
pub(crate) fn try_run_fused_elementwise_into(
    input_flats: &[&[f64]],
    input_shapes: &[&[usize]],
    steps: &[FusedEwStep],
    _out_shape: &[usize],
    out_numel: usize,
    dtype: DType,
    out: &mut Vec<f64>,
) -> MoraResult<bool> {
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (input_flats, input_shapes, steps, out_numel, dtype, out);
        STAT_FALLBACK_DISPATCHES.fetch_add(1, Ordering::Relaxed);
        Ok(false)
    }

    #[cfg(target_arch = "x86_64")]
    {
        x86_64_impl::try_run(input_flats, input_shapes, steps, out_numel, dtype, out)
    }
}

/// P5-2B: Attempt to execute `FusedLinearAct` (`Act(X @ W + bias)`) via strict W^X x86-64
/// AVX2+FMA (`vfmadd231ps` / `vfmadd231pd`) register-blocked `4×8` (`f32`) / `4×4` (`f64`)
/// microkernels. Returns `Ok(true)` if executed natively, or `Ok(false)` to fall back to `simd_kernels`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_run_fused_linear_act_into(
    x_vals: &[f64],
    w_vals: &[f64],
    bias: Option<(&[f64], &[usize])>,
    act: FusedAct,
    m_total: usize,
    k: usize,
    n: usize,
    dtype: DType,
    out: &mut Vec<f64>,
) -> MoraResult<bool> {
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = (x_vals, w_vals, bias, act, m_total, k, n, dtype, out);
        STAT_FALLBACK_DISPATCHES.fetch_add(1, Ordering::Relaxed);
        Ok(false)
    }

    #[cfg(target_arch = "x86_64")]
    {
        x86_64_impl::try_run_fused_linear_act(x_vals, w_vals, bias, act, m_total, k, n, dtype, out)
    }
}

#[cfg(target_arch = "x86_64")]
mod x86_64_impl {
    use std::collections::HashMap;
    use std::ffi::c_void;
    use std::ptr;
    use std::sync::{Arc, OnceLock};

    use crate::common::DType;
    use crate::common::sync::RwLock;
    use crate::error::{MoraError, MoraResult};
    use crate::runtime::compute_dag::{FusedAct, FusedEwStep};
    use crate::value::tensor::quantize_for_dtype;

    use super::{
        Ordering, STAT_CACHE_HITS, STAT_COMPILED_KERNELS, STAT_F32_NATIVE_DISPATCHES,
        STAT_FALLBACK_DISPATCHES, STAT_GEMM_NATIVE_DISPATCHES, STAT_NATIVE_DISPATCHES,
        is_chain_supported, is_enabled,
    };

    /// Uniform C ABI entry signature shared by both Windows x64 and System V AMD64 `f64` kernels.
    type JitEwKernelFn = unsafe extern "C" fn(
        in_ptrs: *const *const f64,
        out_ptr: *mut f64,
        consts_ptr: *const f64,
        len: usize,
    );

    /// G7: Uniform C ABI entry signature for native `8×f32` AVX2 / `1×f32` SSE kernels.
    type JitEwKernelF32Fn = unsafe extern "C" fn(
        in_ptrs: *const *const f32,
        out_ptr: *mut f32,
        consts_ptr: *const f32,
        len: usize,
    );

    // ── Strict W^X Executable Memory Allocation (Windows & Unix, Zero Cargo Deps) ──

    #[cfg(windows)]
    mod os_mem {
        use super::*;

        const MEM_COMMIT: u32 = 0x1000;
        const MEM_RESERVE: u32 = 0x2000;
        const MEM_RELEASE: u32 = 0x8000;
        const PAGE_READWRITE: u32 = 0x04;
        const PAGE_EXECUTE_READ: u32 = 0x20;

        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn VirtualAlloc(
                lp_address: *mut c_void,
                dw_size: usize,
                fl_allocation_type: u32,
                fl_protect: u32,
            ) -> *mut c_void;
            fn VirtualProtect(
                lp_address: *mut c_void,
                dw_size: usize,
                fl_new_protect: u32,
                lpfl_old_protect: *mut u32,
            ) -> i32;
            fn VirtualFree(lp_address: *mut c_void, dw_size: usize, dw_free_type: u32) -> i32;
            fn FlushInstructionCache(
                h_process: *mut c_void,
                lp_base_address: *const c_void,
                dw_size: usize,
            ) -> i32;
            fn GetCurrentProcess() -> *mut c_void;
        }

        pub(super) struct WxCodeBuffer {
            ptr: *mut u8,
            page_len: usize,
        }

        // SAFETY: After construction, `WxCodeBuffer` memory is immutable `PAGE_EXECUTE_READ` (RX)
        // machine code with zero interior mutability, so sharing across threads is safe.
        unsafe impl Send for WxCodeBuffer {}
        // SAFETY: Immutable executable memory page; concurrent execution is read-only.
        unsafe impl Sync for WxCodeBuffer {}

        impl WxCodeBuffer {
            pub(super) fn from_code(code: &[u8]) -> MoraResult<Self> {
                let page_len = code.len().max(1).next_multiple_of(4096);
                // SAFETY: Allocates an anonymous RW memory region via Win32 `VirtualAlloc`
                // (never RWX), copies `code` within bounds, flips strictly RW -> RX via
                // `VirtualProtect`, and flushes the CPU instruction cache.
                unsafe {
                    let raw = VirtualAlloc(
                        ptr::null_mut(),
                        page_len,
                        MEM_COMMIT | MEM_RESERVE,
                        PAGE_READWRITE,
                    );
                    if raw.is_null() {
                        return Err(MoraError::Runtime(
                            "native_jit: VirtualAlloc(PAGE_READWRITE) failed".to_string(),
                        ));
                    }
                    let ptr_u8 = raw as *mut u8;
                    ptr::copy_nonoverlapping(code.as_ptr(), ptr_u8, code.len());
                    let mut old_prot: u32 = 0;
                    if VirtualProtect(raw, page_len, PAGE_EXECUTE_READ, &raw mut old_prot) == 0 {
                        let _ = VirtualFree(raw, 0, MEM_RELEASE);
                        return Err(MoraError::Runtime(
                            "native_jit: VirtualProtect(RW -> RX) failed".to_string(),
                        ));
                    }
                    let _ = FlushInstructionCache(GetCurrentProcess(), raw, code.len());
                    Ok(Self {
                        ptr: ptr_u8,
                        page_len,
                    })
                }
            }

            #[inline]
            pub(super) fn as_ptr(&self) -> *const u8 {
                self.ptr
            }

            #[inline]
            pub(super) fn page_len(&self) -> usize {
                self.page_len
            }
        }

        impl Drop for WxCodeBuffer {
            fn drop(&mut self) {
                if !self.ptr.is_null() {
                    // SAFETY: Releases the page region previously allocated by `VirtualAlloc`.
                    unsafe {
                        let _ = VirtualFree(self.ptr as *mut c_void, 0, MEM_RELEASE);
                    }
                }
            }
        }
    }

    #[cfg(unix)]
    mod os_mem {
        use super::*;

        const PROT_READ: i32 = 0x1;
        const PROT_WRITE: i32 = 0x2;
        const PROT_EXEC: i32 = 0x4;
        const MAP_PRIVATE: i32 = 0x02;
        #[cfg(target_os = "macos")]
        const MAP_ANONYMOUS: i32 = 0x1000;
        #[cfg(not(target_os = "macos"))]
        const MAP_ANONYMOUS: i32 = 0x20;

        unsafe extern "C" {
            fn mmap(
                addr: *mut c_void,
                len: usize,
                prot: i32,
                flags: i32,
                fd: i32,
                offset: isize,
            ) -> *mut c_void;
            fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
            fn munmap(addr: *mut c_void, len: usize) -> i32;
        }

        pub(super) struct WxCodeBuffer {
            ptr: *mut u8,
            page_len: usize,
        }

        // SAFETY: After construction, `WxCodeBuffer` memory is immutable `PROT_READ | PROT_EXEC` (RX).
        unsafe impl Send for WxCodeBuffer {}
        // SAFETY: Read-only executable code pages are safe to execute concurrently from multiple threads.
        unsafe impl Sync for WxCodeBuffer {}

        impl WxCodeBuffer {
            pub(super) fn from_code(code: &[u8]) -> MoraResult<Self> {
                let page_len = code.len().max(1).next_multiple_of(4096);
                // SAFETY: Maps an anonymous private RW region (never RWX), copies `code` within
                // bounds, and transitions strictly RW -> RX via `mprotect`.
                unsafe {
                    let raw = mmap(
                        ptr::null_mut(),
                        page_len,
                        PROT_READ | PROT_WRITE,
                        MAP_PRIVATE | MAP_ANONYMOUS,
                        -1,
                        0,
                    );
                    if raw.is_null() || (raw as isize) == -1 {
                        return Err(MoraError::Runtime(
                            "native_jit: mmap(PROT_READ|PROT_WRITE) failed".to_string(),
                        ));
                    }
                    let ptr_u8 = raw as *mut u8;
                    ptr::copy_nonoverlapping(code.as_ptr(), ptr_u8, code.len());
                    if mprotect(raw, page_len, PROT_READ | PROT_EXEC) != 0 {
                        let _ = munmap(raw, page_len);
                        return Err(MoraError::Runtime(
                            "native_jit: mprotect(RW -> RX) failed".to_string(),
                        ));
                    }
                    Ok(Self {
                        ptr: ptr_u8,
                        page_len,
                    })
                }
            }

            #[inline]
            pub(super) fn as_ptr(&self) -> *const u8 {
                self.ptr
            }

            #[inline]
            pub(super) fn page_len(&self) -> usize {
                self.page_len
            }
        }

        impl Drop for WxCodeBuffer {
            fn drop(&mut self) {
                if !self.ptr.is_null() {
                    // SAFETY: Unmaps the exact region previously mapped by `mmap`.
                    unsafe {
                        let _ = munmap(self.ptr as *mut c_void, self.page_len);
                    }
                }
            }
        }
    }

    struct CompiledNativeKernel {
        code: os_mem::WxCodeBuffer,
        consts: Vec<f64>,
    }

    impl CompiledNativeKernel {
        #[inline]
        fn invoke(&self, in_ptrs: &[*const f64], out: &mut [f64]) {
            if out.is_empty() {
                return;
            }
            let _ = self.code.page_len();
            // SAFETY: `self.code` holds valid, verified x86-64 machine code matching `JitEwKernelFn`
            // in immutable RX memory, `in_ptrs` has length `n_inputs`, every non-broadcast input
            // slice has length `>= out.len()`, every broadcast input slice has length `>= 1`, and
            // `self.consts` holds every constant offset referenced by the emitted instructions.
            unsafe {
                let func: JitEwKernelFn = std::mem::transmute(self.code.as_ptr());
                func(
                    in_ptrs.as_ptr(),
                    out.as_mut_ptr(),
                    self.consts.as_ptr(),
                    out.len(),
                );
            }
        }
    }

    struct CompiledNativeKernelF32 {
        code: os_mem::WxCodeBuffer,
        consts: Vec<f32>,
    }

    impl CompiledNativeKernelF32 {
        #[inline]
        fn invoke(&self, in_ptrs: &[*const f32], out: &mut [f32]) {
            if out.is_empty() {
                return;
            }
            let _ = self.code.page_len();
            // SAFETY: `self.code` holds verified x86-64 `8×f32` AVX2 / `1×f32` SSE machine code
            // matching `JitEwKernelF32Fn` in immutable RX memory, `in_ptrs` has length `n_inputs`,
            // every non-broadcast slice has length `>= out.len()`, every broadcast slice has
            // length `>= 1`, and `self.consts` holds every `f32` constant referenced by the code.
            unsafe {
                let func: JitEwKernelF32Fn = std::mem::transmute(self.code.as_ptr());
                func(
                    in_ptrs.as_ptr(),
                    out.as_mut_ptr(),
                    self.consts.as_ptr(),
                    out.len(),
                );
            }
        }
    }

    // ── Deterministic x86-64 SSE2 + AVX2 Machine-Code Assembler ──

    struct X64Asm {
        buf: Vec<u8>,
    }

    impl X64Asm {
        fn new() -> Self {
            Self {
                buf: Vec::with_capacity(512),
            }
        }

        #[inline]
        fn emit(&mut self, bytes: &[u8]) {
            self.buf.extend_from_slice(bytes);
        }

        #[inline]
        fn emit_i32(&mut self, v: i32) {
            self.buf.extend_from_slice(&v.to_le_bytes());
        }

        #[inline]
        fn pos(&self) -> usize {
            self.buf.len()
        }

        fn patch_rel32(&mut self, patch_offset: usize, target: usize) {
            let next_rip = (patch_offset + 4) as isize;
            let rel = (target as isize - next_rip) as i32;
            self.buf[patch_offset..patch_offset + 4].copy_from_slice(&rel.to_le_bytes());
        }

        /// `mov rdx, [r10 + disp32]`
        fn mov_rdx_r10_disp(&mut self, disp: i32) {
            self.emit(&[0x49, 0x8B, 0x92]);
            self.emit_i32(disp);
        }

        // ── AVX2 (256-bit, 4 × f64) instructions ──

        /// `vbroadcastsd ymm_d, [r8 + disp32]`
        fn vbroadcastsd_r8_disp(&mut self, dst: u8, disp: i32) {
            debug_assert!(dst < 8);
            self.emit(&[0xC4, 0xC2, 0x7D, 0x19, 0x80 | (dst << 3)]);
            self.emit_i32(disp);
        }

        /// `vbroadcastsd ymm_d, [rdx]`
        fn vbroadcastsd_rdx(&mut self, dst: u8) {
            debug_assert!(dst < 8);
            self.emit(&[0xC4, 0xE2, 0x7D, 0x19, (dst << 3) | 0x02]);
        }

        /// `vmovupd ymm_d, [rdx + rax*8]`
        fn vmovupd_load_rdx_rax8(&mut self, dst: u8) {
            debug_assert!(dst < 8);
            self.emit(&[0xC4, 0xE1, 0x7D, 0x10, (dst << 3) | 0x04, 0xC2]);
        }

        /// `vmovupd [r11 + rax*8], ymm_s`
        fn vmovupd_store_r11_rax8(&mut self, src: u8) {
            debug_assert!(src < 8);
            self.emit(&[0xC4, 0xC1, 0x7D, 0x11, (src << 3) | 0x04, 0xC3]);
        }

        /// `vmovapd ymm_d, [rsp + disp32]` (32-byte aligned stack slot)
        fn vmovapd_load_rsp(&mut self, dst: u8, disp: i32) {
            debug_assert!(dst < 8);
            self.emit(&[0xC4, 0xE1, 0x7D, 0x28, 0x84 | (dst << 3), 0x24]);
            self.emit_i32(disp);
        }

        /// `vmovapd [rsp + disp32], ymm_s` (32-byte aligned stack slot)
        fn vmovapd_store_rsp(&mut self, src: u8, disp: i32) {
            debug_assert!(src < 8);
            self.emit(&[0xC4, 0xE1, 0x7D, 0x29, 0x84 | (src << 3), 0x24]);
            self.emit_i32(disp);
        }

        /// 3-operand AVX2 `op ymm_d, ymm_s1, ymm_s2` (VEX.256.66.0F.WIG `opcode` /r)
        fn vex_pd_rrr(&mut self, opcode: u8, dst: u8, src1: u8, src2: u8) {
            debug_assert!(dst < 8 && src1 < 8 && src2 < 8);
            let b2 = ((0x0F ^ src1) << 3) | 0x05; // W=0, ~vvvv, L=1 (256-bit), pp=01 (66)
            self.emit(&[0xC4, 0xE1, b2, opcode, 0xC0 | (dst << 3) | src2]);
        }

        /// 3-operand AVX2 `op ymm_d, ymm_s1, ymm_s2, imm8` (e.g. `vcmppd` opcode `0xC2`)
        fn vex_pd_rrr_imm8(&mut self, opcode: u8, dst: u8, src1: u8, src2: u8, imm8: u8) {
            self.vex_pd_rrr(opcode, dst, src1, src2);
            self.emit(&[imm8]);
        }

        /// `vsqrtpd ymm_d, ymm_s`
        fn vsqrtpd_rr(&mut self, dst: u8, src: u8) {
            debug_assert!(dst < 8 && src < 8);
            self.emit(&[0xC4, 0xE1, 0x7D, 0x51, 0xC0 | (dst << 3) | src]);
        }

        /// `vzeroupper`
        fn vzeroupper(&mut self) {
            self.emit(&[0xC5, 0xF8, 0x77]);
        }

        // ── SSE2 (64-bit scalar `f64`) instructions ──

        /// `movsd xmm_d, [r8 + disp32]`
        fn movsd_load_r8_disp(&mut self, dst: u8, disp: i32) {
            debug_assert!(dst < 8);
            self.emit(&[0xF2, 0x41, 0x0F, 0x10, 0x80 | (dst << 3)]);
            self.emit_i32(disp);
        }

        /// `movsd xmm_d, [rdx]`
        fn movsd_load_rdx(&mut self, dst: u8) {
            debug_assert!(dst < 8);
            self.emit(&[0xF2, 0x0F, 0x10, (dst << 3) | 0x02]);
        }

        /// `movsd xmm_d, [rdx + rax*8]`
        fn movsd_load_rdx_rax8(&mut self, dst: u8) {
            debug_assert!(dst < 8);
            self.emit(&[0xF2, 0x0F, 0x10, (dst << 3) | 0x04, 0xC2]);
        }

        /// `movsd [r11 + rax*8], xmm_s`
        fn movsd_store_r11_rax8(&mut self, src: u8) {
            debug_assert!(src < 8);
            self.emit(&[0xF2, 0x41, 0x0F, 0x11, (src << 3) | 0x04, 0xC3]);
        }

        /// `movsd xmm_d, [rsp + disp32]`
        fn movsd_load_rsp(&mut self, dst: u8, disp: i32) {
            debug_assert!(dst < 8);
            self.emit(&[0xF2, 0x0F, 0x10, 0x84 | (dst << 3), 0x24]);
            self.emit_i32(disp);
        }

        /// `movsd [rsp + disp32], xmm_s`
        fn movsd_store_rsp(&mut self, src: u8, disp: i32) {
            debug_assert!(src < 8);
            self.emit(&[0xF2, 0x0F, 0x11, 0x84 | (src << 3), 0x24]);
            self.emit_i32(disp);
        }

        /// `op xmm_d, xmm_s` (`F2 0F opcode ModRM`)
        fn sse2_sd_rr(&mut self, opcode: u8, dst: u8, src: u8) {
            debug_assert!(dst < 8 && src < 8);
            self.emit(&[0xF2, 0x0F, opcode, 0xC0 | (dst << 3) | src]);
        }

        /// `op xmm_d, xmm_s, imm8` (`F2 0F opcode ModRM imm8`, e.g. `cmpsd` `0xC2`)
        fn sse2_sd_rr_imm8(&mut self, opcode: u8, dst: u8, src: u8, imm8: u8) {
            self.sse2_sd_rr(opcode, dst, src);
            self.emit(&[imm8]);
        }

        /// `op xmm_d, xmm_s` (`66 0F opcode ModRM` — `xorpd`/`andpd`)
        fn sse2_pd_rr(&mut self, opcode: u8, dst: u8, src: u8) {
            debug_assert!(dst < 8 && src < 8);
            self.emit(&[0x66, 0x0F, opcode, 0xC0 | (dst << 3) | src]);
        }

        // ── G7: AVX2 (256-bit, 8 × f32) instructions ──

        /// `vbroadcastss ymm_d, [r8 + disp32]` (VEX.256.66.0F38.W0 18 /r)
        fn vbroadcastss_r8_disp(&mut self, dst: u8, disp: i32) {
            debug_assert!(dst < 8);
            self.emit(&[0xC4, 0xC2, 0x7D, 0x18, 0x80 | (dst << 3)]);
            self.emit_i32(disp);
        }

        /// `vbroadcastss ymm_d, [rdx]`
        fn vbroadcastss_rdx(&mut self, dst: u8) {
            debug_assert!(dst < 8);
            self.emit(&[0xC4, 0xE2, 0x7D, 0x18, (dst << 3) | 0x02]);
        }

        /// `vmovups ymm_d, [rdx + rax*4]` (VEX.256.NP.0F.WIG 10 /r)
        fn vmovups_load_rdx_rax4(&mut self, dst: u8) {
            debug_assert!(dst < 8);
            self.emit(&[0xC4, 0xE1, 0x7C, 0x10, (dst << 3) | 0x04, 0x82]);
        }

        /// `vmovups [r11 + rax*4], ymm_s` (VEX.256.NP.0F.WIG 11 /r)
        fn vmovups_store_r11_rax4(&mut self, src: u8) {
            debug_assert!(src < 8);
            self.emit(&[0xC4, 0xC1, 0x7C, 0x11, (src << 3) | 0x04, 0x83]);
        }

        /// `vmovaps ymm_d, [rsp + disp32]` (32-byte aligned stack slot)
        fn vmovaps_load_rsp(&mut self, dst: u8, disp: i32) {
            debug_assert!(dst < 8);
            self.emit(&[0xC4, 0xE1, 0x7C, 0x28, 0x84 | (dst << 3), 0x24]);
            self.emit_i32(disp);
        }

        /// `vmovaps [rsp + disp32], ymm_s` (32-byte aligned stack slot)
        fn vmovaps_store_rsp(&mut self, src: u8, disp: i32) {
            debug_assert!(src < 8);
            self.emit(&[0xC4, 0xE1, 0x7C, 0x29, 0x84 | (src << 3), 0x24]);
            self.emit_i32(disp);
        }

        /// 3-operand AVX2 `op ymm_d, ymm_s1, ymm_s2` (VEX.256.NP.0F.WIG `opcode` /r)
        fn vex_ps_rrr(&mut self, opcode: u8, dst: u8, src1: u8, src2: u8) {
            debug_assert!(dst < 8 && src1 < 8 && src2 < 8);
            let b2 = ((0x0F ^ src1) << 3) | 0x04; // W=0, ~vvvv, L=1 (256-bit), pp=00 (NP)
            self.emit(&[0xC4, 0xE1, b2, opcode, 0xC0 | (dst << 3) | src2]);
        }

        /// 3-operand AVX2 `op ymm_d, ymm_s1, ymm_s2, imm8` (e.g. `vcmpps` opcode `0xC2`)
        fn vex_ps_rrr_imm8(&mut self, opcode: u8, dst: u8, src1: u8, src2: u8, imm8: u8) {
            self.vex_ps_rrr(opcode, dst, src1, src2);
            self.emit(&[imm8]);
        }

        /// `vsqrtps ymm_d, ymm_s`
        fn vsqrtps_rr(&mut self, dst: u8, src: u8) {
            debug_assert!(dst < 8 && src < 8);
            self.emit(&[0xC4, 0xE1, 0x7C, 0x51, 0xC0 | (dst << 3) | src]);
        }

        // ── G7: SSE (32-bit scalar `f32`) instructions ──

        /// `movss xmm_d, [r8 + disp32]`
        fn movss_load_r8_disp(&mut self, dst: u8, disp: i32) {
            debug_assert!(dst < 8);
            self.emit(&[0xF3, 0x41, 0x0F, 0x10, 0x80 | (dst << 3)]);
            self.emit_i32(disp);
        }

        /// `movss xmm_d, [rdx]`
        fn movss_load_rdx(&mut self, dst: u8) {
            debug_assert!(dst < 8);
            self.emit(&[0xF3, 0x0F, 0x10, (dst << 3) | 0x02]);
        }

        /// `movss xmm_d, [rdx + rax*4]`
        fn movss_load_rdx_rax4(&mut self, dst: u8) {
            debug_assert!(dst < 8);
            self.emit(&[0xF3, 0x0F, 0x10, (dst << 3) | 0x04, 0x82]);
        }

        /// `movss [r11 + rax*4], xmm_s`
        fn movss_store_r11_rax4(&mut self, src: u8) {
            debug_assert!(src < 8);
            self.emit(&[0xF3, 0x41, 0x0F, 0x11, (src << 3) | 0x04, 0x83]);
        }

        /// `movss xmm_d, [rsp + disp32]`
        fn movss_load_rsp(&mut self, dst: u8, disp: i32) {
            debug_assert!(dst < 8);
            self.emit(&[0xF3, 0x0F, 0x10, 0x84 | (dst << 3), 0x24]);
            self.emit_i32(disp);
        }

        /// `movss [rsp + disp32], xmm_s`
        fn movss_store_rsp(&mut self, src: u8, disp: i32) {
            debug_assert!(src < 8);
            self.emit(&[0xF3, 0x0F, 0x11, 0x84 | (src << 3), 0x24]);
            self.emit_i32(disp);
        }

        /// `op xmm_d, xmm_s` (`F3 0F opcode ModRM` — `addss`/`subss`/`mulss`/`divss`/`sqrtss`/`maxss`)
        fn sse_ss_rr(&mut self, opcode: u8, dst: u8, src: u8) {
            debug_assert!(dst < 8 && src < 8);
            self.emit(&[0xF3, 0x0F, opcode, 0xC0 | (dst << 3) | src]);
        }

        /// `op xmm_d, xmm_s, imm8` (`F3 0F opcode ModRM imm8`, e.g. `cmpss` `0xC2`)
        fn sse_ss_rr_imm8(&mut self, opcode: u8, dst: u8, src: u8, imm8: u8) {
            self.sse_ss_rr(opcode, dst, src);
            self.emit(&[imm8]);
        }

        /// `op xmm_d, xmm_s` (`0F opcode ModRM` — `xorps`/`andps`)
        fn sse_ps_rr(&mut self, opcode: u8, dst: u8, src: u8) {
            debug_assert!(dst < 8 && src < 8);
            self.emit(&[0x0F, opcode, 0xC0 | (dst << 3) | src]);
        }
    }

    fn emit_kernel_code(
        n_inputs: usize,
        broadcast_mask: u64,
        steps: &[FusedEwStep],
        use_avx2: bool,
    ) -> (Vec<u8>, Vec<f64>) {
        // Reserve consts[0]=+0.0, consts[1]=sign_mask, consts[2]=abs_mask, consts[3]=+1.0, then step immediates.
        let mut consts = vec![
            0.0_f64,
            f64::from_bits(0x8000_0000_0000_0000),
            f64::from_bits(0x7FFF_FFFF_FFFF_FFFF),
            1.0_f64,
        ];
        let mut step_const_idx: Vec<usize> = Vec::with_capacity(steps.len());
        for step in steps {
            let c_opt = match *step {
                FusedEwStep::AddScalar(_, c)
                | FusedEwStep::SubScalar(_, c, _)
                | FusedEwStep::MulScalar(_, c)
                | FusedEwStep::DivScalar(_, c, _) => Some(c),
                _ => None,
            };
            if let Some(c) = c_opt {
                let idx = consts.len();
                consts.push(c);
                step_const_idx.push(idx);
            } else {
                step_const_idx.push(0);
            }
        }

        let total_slots = n_inputs + steps.len();
        let frame_bytes = (total_slots * 32).max(32) as i32;
        let slot_off = |slot: usize| -> i32 { (slot * 32) as i32 };

        let mut a = X64Asm::new();

        // Prologue: establish 32-byte aligned stack scratch frame.
        a.emit(&[0x55]); // push rbp
        a.emit(&[0x48, 0x89, 0xE5]); // mov rbp, rsp
        a.emit(&[0x48, 0x83, 0xE4, 0xE0]); // and rsp, -32
        a.emit(&[0x48, 0x81, 0xEC]); // sub rsp, frame_bytes
        a.emit_i32(frame_bytes);

        // Normalize calling convention args into (r10=in_ptrs, r11=out_ptr, r8=consts_ptr, r9=len).
        #[cfg(windows)]
        {
            a.emit(&[0x49, 0x89, 0xCA]); // mov r10, rcx
            a.emit(&[0x49, 0x89, 0xD3]); // mov r11, rdx
        }
        #[cfg(unix)]
        {
            a.emit(&[0x49, 0x89, 0xFA]); // mov r10, rdi
            a.emit(&[0x49, 0x89, 0xF3]); // mov r11, rsi
            a.emit(&[0x49, 0x89, 0xD0]); // mov r8, rdx
            a.emit(&[0x49, 0x89, 0xC9]); // mov r9, rcx
        }

        // Loop counter `rax = 0`.
        a.emit(&[0x31, 0xC0]); // xor eax, eax

        // ── Optional AVX2 4×f64 Vector Loop ──
        if use_avx2 {
            a.vbroadcastsd_r8_disp(2, 0); // ymm2 = [+0.0; 4]
            a.vbroadcastsd_r8_disp(3, 8); // ymm3 = [sign_mask; 4]
            a.vbroadcastsd_r8_disp(4, 16); // ymm4 = [abs_mask; 4]
            a.vbroadcastsd_r8_disp(5, 24); // ymm5 = [+1.0; 4]

            let avx_loop = a.pos();
            a.emit(&[0x48, 0x8D, 0x48, 0x04]); // lea rcx, [rax + 4]
            a.emit(&[0x4C, 0x39, 0xC9]); // cmp rcx, r9
            a.emit(&[0x0F, 0x87]); // ja avx_done
            let avx_exit_patch = a.pos();
            a.emit_i32(0);

            // Load inputs into stack slots 0..n_inputs.
            for i in 0..n_inputs {
                a.mov_rdx_r10_disp((i * 8) as i32);
                if (broadcast_mask & (1u64 << i)) != 0 {
                    a.vbroadcastsd_rdx(0);
                } else {
                    a.vmovupd_load_rdx_rax8(0);
                }
                a.vmovapd_store_rsp(0, slot_off(i));
            }

            // Evaluate fused steps in topological order.
            for (s_idx, step) in steps.iter().enumerate() {
                let dst_slot = n_inputs + s_idx;
                let c_disp = (step_const_idx[s_idx] * 8) as i32;
                match *step {
                    FusedEwStep::Add(lhs, rhs) => {
                        a.vmovapd_load_rsp(0, slot_off(lhs));
                        a.vmovapd_load_rsp(1, slot_off(rhs));
                        a.vex_pd_rrr(0x58, 0, 0, 1); // vaddpd ymm0, ymm0, ymm1
                    }
                    FusedEwStep::Sub(lhs, rhs) => {
                        a.vmovapd_load_rsp(0, slot_off(lhs));
                        a.vmovapd_load_rsp(1, slot_off(rhs));
                        a.vex_pd_rrr(0x5C, 0, 0, 1); // vsubpd ymm0, ymm0, ymm1
                    }
                    FusedEwStep::Mul(lhs, rhs) => {
                        a.vmovapd_load_rsp(0, slot_off(lhs));
                        a.vmovapd_load_rsp(1, slot_off(rhs));
                        a.vex_pd_rrr(0x59, 0, 0, 1); // vmulpd ymm0, ymm0, ymm1
                    }
                    FusedEwStep::AddScalar(lhs, _) => {
                        a.vmovapd_load_rsp(0, slot_off(lhs));
                        a.vbroadcastsd_r8_disp(1, c_disp);
                        a.vex_pd_rrr(0x58, 0, 0, 1);
                    }
                    FusedEwStep::SubScalar(lhs, _, false) => {
                        a.vmovapd_load_rsp(0, slot_off(lhs));
                        a.vbroadcastsd_r8_disp(1, c_disp);
                        a.vex_pd_rrr(0x5C, 0, 0, 1);
                    }
                    FusedEwStep::SubScalar(lhs, _, true) => {
                        a.vbroadcastsd_r8_disp(0, c_disp);
                        a.vmovapd_load_rsp(1, slot_off(lhs));
                        a.vex_pd_rrr(0x5C, 0, 0, 1);
                    }
                    FusedEwStep::MulScalar(lhs, _) => {
                        a.vmovapd_load_rsp(0, slot_off(lhs));
                        a.vbroadcastsd_r8_disp(1, c_disp);
                        a.vex_pd_rrr(0x59, 0, 0, 1);
                    }
                    FusedEwStep::DivScalar(lhs, _, false) => {
                        a.vmovapd_load_rsp(0, slot_off(lhs));
                        a.vbroadcastsd_r8_disp(1, c_disp);
                        a.vex_pd_rrr(0x5E, 0, 0, 1); // vdivpd ymm0, ymm0, ymm1
                    }
                    FusedEwStep::Neg(lhs) => {
                        a.vmovapd_load_rsp(0, slot_off(lhs));
                        a.vex_pd_rrr(0x57, 0, 0, 3); // vxorpd ymm0, ymm0, ymm3 (sign mask)
                    }
                    FusedEwStep::Abs(lhs) => {
                        a.vmovapd_load_rsp(0, slot_off(lhs));
                        a.vex_pd_rrr(0x54, 0, 0, 4); // vandpd ymm0, ymm0, ymm4 (abs mask)
                    }
                    FusedEwStep::Sqrt(lhs) => {
                        a.vmovapd_load_rsp(0, slot_off(lhs));
                        a.vsqrtpd_rr(0, 0);
                    }
                    FusedEwStep::Relu(lhs) => {
                        // `vmaxpd ymm0, ymm0, ymm2` (src2 = +0.0) matches `if x > 0.0 { x } else { 0.0 }`
                        // bit-for-bit across all IEEE-754 inputs including `-0.0`, `±Inf`, and `NaN`.
                        a.vmovapd_load_rsp(0, slot_off(lhs));
                        a.vex_pd_rrr(0x5F, 0, 0, 2);
                    }
                    FusedEwStep::StepPos(lhs) => {
                        // `(0.0 < x) & 1.0`: ordered LT mask returns 1.0 iff `x > 0.0`, else 0.0 (incl. NaN).
                        a.vmovapd_load_rsp(1, slot_off(lhs));
                        a.vex_pd_rrr_imm8(0xC2, 0, 2, 1, 0x01); // vcmppd ymm0, ymm2 (+0.0), ymm1 (x), LT
                        a.vex_pd_rrr(0x54, 0, 0, 5); // vandpd ymm0, ymm0, ymm5 (+1.0)
                    }
                    FusedEwStep::Sign(lhs) => {
                        // `((0.0 < x) & 1.0) - ((x < 0.0) & 1.0)` matches exact signum for all IEEE-754 inputs.
                        a.vmovapd_load_rsp(1, slot_off(lhs));
                        a.vex_pd_rrr_imm8(0xC2, 0, 2, 1, 0x01); // ymm0 = (0.0 < x)
                        a.vex_pd_rrr(0x54, 0, 0, 5); // ymm0 = pos
                        a.vex_pd_rrr_imm8(0xC2, 1, 1, 2, 0x01); // ymm1 = (x < 0.0)
                        a.vex_pd_rrr(0x54, 1, 1, 5); // ymm1 = neg
                        a.vex_pd_rrr(0x5C, 0, 0, 1); // ymm0 = pos - neg
                    }
                    _ => unreachable!("is_chain_supported pre-filtered unsupported step"),
                }
                a.vmovapd_store_rsp(0, slot_off(dst_slot));
            }

            // Store final output vector `out[rax..rax+4] = slot[last]`.
            a.vmovupd_store_r11_rax8(0);
            a.emit(&[0x48, 0x83, 0xC0, 0x04]); // add rax, 4
            a.emit(&[0xE9]); // jmp avx_loop
            let back_patch = a.pos();
            a.emit_i32(0);
            a.patch_rel32(back_patch, avx_loop);

            let avx_done = a.pos();
            a.patch_rel32(avx_exit_patch, avx_done);
            a.vzeroupper();
        }

        // ── SSE2 Scalar Tail Loop (`1 × f64` per iteration) ──
        a.movsd_load_r8_disp(2, 0); // xmm2 = +0.0
        a.movsd_load_r8_disp(3, 8); // xmm3 = sign_mask
        a.movsd_load_r8_disp(4, 16); // xmm4 = abs_mask
        a.movsd_load_r8_disp(5, 24); // xmm5 = +1.0

        let scalar_loop = a.pos();
        a.emit(&[0x4C, 0x39, 0xC8]); // cmp rax, r9
        a.emit(&[0x0F, 0x83]); // jae scalar_done
        let scalar_exit_patch = a.pos();
        a.emit_i32(0);

        for i in 0..n_inputs {
            a.mov_rdx_r10_disp((i * 8) as i32);
            if (broadcast_mask & (1u64 << i)) != 0 {
                a.movsd_load_rdx(0);
            } else {
                a.movsd_load_rdx_rax8(0);
            }
            a.movsd_store_rsp(0, slot_off(i));
        }

        for (s_idx, step) in steps.iter().enumerate() {
            let dst_slot = n_inputs + s_idx;
            let c_disp = (step_const_idx[s_idx] * 8) as i32;
            match *step {
                FusedEwStep::Add(lhs, rhs) => {
                    a.movsd_load_rsp(0, slot_off(lhs));
                    a.movsd_load_rsp(1, slot_off(rhs));
                    a.sse2_sd_rr(0x58, 0, 1); // addsd
                }
                FusedEwStep::Sub(lhs, rhs) => {
                    a.movsd_load_rsp(0, slot_off(lhs));
                    a.movsd_load_rsp(1, slot_off(rhs));
                    a.sse2_sd_rr(0x5C, 0, 1); // subsd
                }
                FusedEwStep::Mul(lhs, rhs) => {
                    a.movsd_load_rsp(0, slot_off(lhs));
                    a.movsd_load_rsp(1, slot_off(rhs));
                    a.sse2_sd_rr(0x59, 0, 1); // mulsd
                }
                FusedEwStep::AddScalar(lhs, _) => {
                    a.movsd_load_rsp(0, slot_off(lhs));
                    a.movsd_load_r8_disp(1, c_disp);
                    a.sse2_sd_rr(0x58, 0, 1);
                }
                FusedEwStep::SubScalar(lhs, _, false) => {
                    a.movsd_load_rsp(0, slot_off(lhs));
                    a.movsd_load_r8_disp(1, c_disp);
                    a.sse2_sd_rr(0x5C, 0, 1);
                }
                FusedEwStep::SubScalar(lhs, _, true) => {
                    a.movsd_load_r8_disp(0, c_disp);
                    a.movsd_load_rsp(1, slot_off(lhs));
                    a.sse2_sd_rr(0x5C, 0, 1);
                }
                FusedEwStep::MulScalar(lhs, _) => {
                    a.movsd_load_rsp(0, slot_off(lhs));
                    a.movsd_load_r8_disp(1, c_disp);
                    a.sse2_sd_rr(0x59, 0, 1);
                }
                FusedEwStep::DivScalar(lhs, _, false) => {
                    a.movsd_load_rsp(0, slot_off(lhs));
                    a.movsd_load_r8_disp(1, c_disp);
                    a.sse2_sd_rr(0x5E, 0, 1); // divsd
                }
                FusedEwStep::Neg(lhs) => {
                    a.movsd_load_rsp(0, slot_off(lhs));
                    a.sse2_pd_rr(0x57, 0, 3); // xorpd xmm0, xmm3
                }
                FusedEwStep::Abs(lhs) => {
                    a.movsd_load_rsp(0, slot_off(lhs));
                    a.sse2_pd_rr(0x54, 0, 4); // andpd xmm0, xmm4
                }
                FusedEwStep::Sqrt(lhs) => {
                    a.movsd_load_rsp(0, slot_off(lhs));
                    a.sse2_sd_rr(0x51, 0, 0); // sqrtsd xmm0, xmm0
                }
                FusedEwStep::Relu(lhs) => {
                    a.movsd_load_rsp(0, slot_off(lhs));
                    a.sse2_sd_rr(0x5F, 0, 2); // maxsd xmm0, xmm2 (+0.0)
                }
                FusedEwStep::StepPos(lhs) => {
                    a.sse2_pd_rr(0x28, 0, 2); // movapd xmm0, xmm2 (+0.0)
                    a.movsd_load_rsp(1, slot_off(lhs));
                    a.sse2_sd_rr_imm8(0xC2, 0, 1, 0x01); // cmpsd xmm0, xmm1, LT
                    a.sse2_pd_rr(0x54, 0, 5); // andpd xmm0, xmm5 (+1.0)
                }
                FusedEwStep::Sign(lhs) => {
                    a.sse2_pd_rr(0x28, 0, 2); // movapd xmm0, xmm2 (+0.0)
                    a.movsd_load_rsp(1, slot_off(lhs));
                    a.sse2_sd_rr_imm8(0xC2, 0, 1, 0x01); // xmm0 = (0.0 < x)
                    a.sse2_pd_rr(0x54, 0, 5); // xmm0 = pos
                    a.sse2_sd_rr_imm8(0xC2, 1, 2, 0x01); // xmm1 = (x < 0.0)
                    a.sse2_pd_rr(0x54, 1, 5); // xmm1 = neg
                    a.sse2_sd_rr(0x5C, 0, 1); // subsd xmm0, xmm1
                }
                _ => unreachable!("is_chain_supported pre-filtered unsupported step"),
            }
            a.movsd_store_rsp(0, slot_off(dst_slot));
        }

        a.movsd_store_r11_rax8(0);
        a.emit(&[0x48, 0x83, 0xC0, 0x01]); // add rax, 1
        a.emit(&[0xE9]); // jmp scalar_loop
        let s_back_patch = a.pos();
        a.emit_i32(0);
        a.patch_rel32(s_back_patch, scalar_loop);

        let scalar_done = a.pos();
        a.patch_rel32(scalar_exit_patch, scalar_done);

        // Epilogue.
        a.emit(&[0x48, 0x89, 0xEC]); // mov rsp, rbp
        a.emit(&[0x5D]); // pop rbp
        a.emit(&[0xC3]); // ret

        (a.buf, consts)
    }

    /// G7: Emit strict W^X x86-64 AVX2 `8×f32` vector loop + SSE `1×f32` scalar tail kernel.
    fn emit_kernel_code_f32(
        n_inputs: usize,
        broadcast_mask: u64,
        steps: &[FusedEwStep],
        use_avx2: bool,
    ) -> (Vec<u8>, Vec<f32>) {
        // Reserve consts[0]=+0.0f, consts[1]=sign_mask, consts[2]=abs_mask, consts[3]=+1.0f, then step immediates.
        let mut consts = vec![
            0.0_f32,
            f32::from_bits(0x8000_0000),
            f32::from_bits(0x7FFF_FFFF),
            1.0_f32,
        ];
        let mut step_const_idx: Vec<usize> = Vec::with_capacity(steps.len());
        for step in steps {
            let c_opt = match *step {
                FusedEwStep::AddScalar(_, c)
                | FusedEwStep::SubScalar(_, c, _)
                | FusedEwStep::MulScalar(_, c)
                | FusedEwStep::DivScalar(_, c, _) => Some(c as f32),
                _ => None,
            };
            if let Some(c) = c_opt {
                let idx = consts.len();
                consts.push(c);
                step_const_idx.push(idx);
            } else {
                step_const_idx.push(0);
            }
        }

        let total_slots = n_inputs + steps.len();
        let frame_bytes = (total_slots * 32).max(32) as i32;
        let slot_off = |slot: usize| -> i32 { (slot * 32) as i32 };

        let mut a = X64Asm::new();

        // Prologue: 32-byte aligned stack scratchpad.
        a.emit(&[0x55]); // push rbp
        a.emit(&[0x48, 0x89, 0xE5]); // mov rbp, rsp
        a.emit(&[0x48, 0x83, 0xE4, 0xE0]); // and rsp, -32
        a.emit(&[0x48, 0x81, 0xEC]); // sub rsp, frame_bytes
        a.emit_i32(frame_bytes);

        // Normalize calling convention args into (r10=in_ptrs, r11=out_ptr, r8=consts_ptr, r9=len).
        #[cfg(windows)]
        {
            a.emit(&[0x49, 0x89, 0xCA]); // mov r10, rcx
            a.emit(&[0x49, 0x89, 0xD3]); // mov r11, rdx
        }
        #[cfg(unix)]
        {
            a.emit(&[0x49, 0x89, 0xFA]); // mov r10, rdi
            a.emit(&[0x49, 0x89, 0xF3]); // mov r11, rsi
            a.emit(&[0x49, 0x89, 0xD0]); // mov r8, rdx
            a.emit(&[0x49, 0x89, 0xC9]); // mov r9, rcx
        }

        // Loop counter `rax = 0`.
        a.emit(&[0x31, 0xC0]); // xor eax, eax

        // ── Optional AVX2 8×f32 Vector Loop ──
        if use_avx2 {
            a.vbroadcastss_r8_disp(2, 0); // ymm2 = [+0.0f; 8]
            a.vbroadcastss_r8_disp(3, 4); // ymm3 = [sign_mask; 8]
            a.vbroadcastss_r8_disp(4, 8); // ymm4 = [abs_mask; 8]
            a.vbroadcastss_r8_disp(5, 12); // ymm5 = [+1.0f; 8]

            let avx_loop = a.pos();
            a.emit(&[0x48, 0x8D, 0x48, 0x08]); // lea rcx, [rax + 8]
            a.emit(&[0x4C, 0x39, 0xC9]); // cmp rcx, r9
            a.emit(&[0x0F, 0x87]); // ja avx_done
            let avx_exit_patch = a.pos();
            a.emit_i32(0);

            for i in 0..n_inputs {
                a.mov_rdx_r10_disp((i * 8) as i32);
                if (broadcast_mask & (1u64 << i)) != 0 {
                    a.vbroadcastss_rdx(0);
                } else {
                    a.vmovups_load_rdx_rax4(0);
                }
                a.vmovaps_store_rsp(0, slot_off(i));
            }

            for (s_idx, step) in steps.iter().enumerate() {
                let dst_slot = n_inputs + s_idx;
                let c_disp = (step_const_idx[s_idx] * 4) as i32;
                match *step {
                    FusedEwStep::Add(lhs, rhs) => {
                        a.vmovaps_load_rsp(0, slot_off(lhs));
                        a.vmovaps_load_rsp(1, slot_off(rhs));
                        a.vex_ps_rrr(0x58, 0, 0, 1); // vaddps
                    }
                    FusedEwStep::Sub(lhs, rhs) => {
                        a.vmovaps_load_rsp(0, slot_off(lhs));
                        a.vmovaps_load_rsp(1, slot_off(rhs));
                        a.vex_ps_rrr(0x5C, 0, 0, 1); // vsubps
                    }
                    FusedEwStep::Mul(lhs, rhs) => {
                        a.vmovaps_load_rsp(0, slot_off(lhs));
                        a.vmovaps_load_rsp(1, slot_off(rhs));
                        a.vex_ps_rrr(0x59, 0, 0, 1); // vmulps
                    }
                    FusedEwStep::AddScalar(lhs, _) => {
                        a.vmovaps_load_rsp(0, slot_off(lhs));
                        a.vbroadcastss_r8_disp(1, c_disp);
                        a.vex_ps_rrr(0x58, 0, 0, 1);
                    }
                    FusedEwStep::SubScalar(lhs, _, false) => {
                        a.vmovaps_load_rsp(0, slot_off(lhs));
                        a.vbroadcastss_r8_disp(1, c_disp);
                        a.vex_ps_rrr(0x5C, 0, 0, 1);
                    }
                    FusedEwStep::SubScalar(lhs, _, true) => {
                        a.vbroadcastss_r8_disp(0, c_disp);
                        a.vmovaps_load_rsp(1, slot_off(lhs));
                        a.vex_ps_rrr(0x5C, 0, 0, 1);
                    }
                    FusedEwStep::MulScalar(lhs, _) => {
                        a.vmovaps_load_rsp(0, slot_off(lhs));
                        a.vbroadcastss_r8_disp(1, c_disp);
                        a.vex_ps_rrr(0x59, 0, 0, 1);
                    }
                    FusedEwStep::DivScalar(lhs, _, false) => {
                        a.vmovaps_load_rsp(0, slot_off(lhs));
                        a.vbroadcastss_r8_disp(1, c_disp);
                        a.vex_ps_rrr(0x5E, 0, 0, 1); // vdivps
                    }
                    FusedEwStep::Neg(lhs) => {
                        a.vmovaps_load_rsp(0, slot_off(lhs));
                        a.vex_ps_rrr(0x57, 0, 0, 3); // vxorps
                    }
                    FusedEwStep::Abs(lhs) => {
                        a.vmovaps_load_rsp(0, slot_off(lhs));
                        a.vex_ps_rrr(0x54, 0, 0, 4); // vandps
                    }
                    FusedEwStep::Sqrt(lhs) => {
                        a.vmovaps_load_rsp(0, slot_off(lhs));
                        a.vsqrtps_rr(0, 0);
                    }
                    FusedEwStep::Relu(lhs) => {
                        a.vmovaps_load_rsp(0, slot_off(lhs));
                        a.vex_ps_rrr(0x5F, 0, 0, 2); // vmaxps ymm0, ymm0, +0.0f
                    }
                    FusedEwStep::StepPos(lhs) => {
                        a.vmovaps_load_rsp(1, slot_off(lhs));
                        a.vex_ps_rrr_imm8(0xC2, 0, 2, 1, 0x01); // vcmpps ymm0, ymm2 (+0.0f), ymm1 (x), LT
                        a.vex_ps_rrr(0x54, 0, 0, 5); // vandps ymm0, ymm0, ymm5 (+1.0f)
                    }
                    FusedEwStep::Sign(lhs) => {
                        a.vmovaps_load_rsp(1, slot_off(lhs));
                        a.vex_ps_rrr_imm8(0xC2, 0, 2, 1, 0x01); // ymm0 = (0.0f < x)
                        a.vex_ps_rrr(0x54, 0, 0, 5); // ymm0 = pos
                        a.vex_ps_rrr_imm8(0xC2, 1, 1, 2, 0x01); // ymm1 = (x < 0.0f)
                        a.vex_ps_rrr(0x54, 1, 1, 5); // ymm1 = neg
                        a.vex_ps_rrr(0x5C, 0, 0, 1); // ymm0 = pos - neg
                    }
                    _ => unreachable!("is_chain_supported pre-filtered unsupported step"),
                }
                a.vmovaps_store_rsp(0, slot_off(dst_slot));
            }

            a.vmovups_store_r11_rax4(0);
            a.emit(&[0x48, 0x83, 0xC0, 0x08]); // add rax, 8
            a.emit(&[0xE9]); // jmp avx_loop
            let back_patch = a.pos();
            a.emit_i32(0);
            a.patch_rel32(back_patch, avx_loop);

            let avx_done = a.pos();
            a.patch_rel32(avx_exit_patch, avx_done);
            a.vzeroupper();
        }

        // ── SSE Scalar Tail Loop (`1 × f32` per iteration) ──
        a.movss_load_r8_disp(2, 0); // xmm2 = +0.0f
        a.movss_load_r8_disp(3, 4); // xmm3 = sign_mask
        a.movss_load_r8_disp(4, 8); // xmm4 = abs_mask
        a.movss_load_r8_disp(5, 12); // xmm5 = +1.0f

        let scalar_loop = a.pos();
        a.emit(&[0x4C, 0x39, 0xC8]); // cmp rax, r9
        a.emit(&[0x0F, 0x83]); // jae scalar_done
        let scalar_exit_patch = a.pos();
        a.emit_i32(0);

        for i in 0..n_inputs {
            a.mov_rdx_r10_disp((i * 8) as i32);
            if (broadcast_mask & (1u64 << i)) != 0 {
                a.movss_load_rdx(0);
            } else {
                a.movss_load_rdx_rax4(0);
            }
            a.movss_store_rsp(0, slot_off(i));
        }

        for (s_idx, step) in steps.iter().enumerate() {
            let dst_slot = n_inputs + s_idx;
            let c_disp = (step_const_idx[s_idx] * 4) as i32;
            match *step {
                FusedEwStep::Add(lhs, rhs) => {
                    a.movss_load_rsp(0, slot_off(lhs));
                    a.movss_load_rsp(1, slot_off(rhs));
                    a.sse_ss_rr(0x58, 0, 1); // addss
                }
                FusedEwStep::Sub(lhs, rhs) => {
                    a.movss_load_rsp(0, slot_off(lhs));
                    a.movss_load_rsp(1, slot_off(rhs));
                    a.sse_ss_rr(0x5C, 0, 1); // subss
                }
                FusedEwStep::Mul(lhs, rhs) => {
                    a.movss_load_rsp(0, slot_off(lhs));
                    a.movss_load_rsp(1, slot_off(rhs));
                    a.sse_ss_rr(0x59, 0, 1); // mulss
                }
                FusedEwStep::AddScalar(lhs, _) => {
                    a.movss_load_rsp(0, slot_off(lhs));
                    a.movss_load_r8_disp(1, c_disp);
                    a.sse_ss_rr(0x58, 0, 1);
                }
                FusedEwStep::SubScalar(lhs, _, false) => {
                    a.movss_load_rsp(0, slot_off(lhs));
                    a.movss_load_r8_disp(1, c_disp);
                    a.sse_ss_rr(0x5C, 0, 1);
                }
                FusedEwStep::SubScalar(lhs, _, true) => {
                    a.movss_load_r8_disp(0, c_disp);
                    a.movss_load_rsp(1, slot_off(lhs));
                    a.sse_ss_rr(0x5C, 0, 1);
                }
                FusedEwStep::MulScalar(lhs, _) => {
                    a.movss_load_rsp(0, slot_off(lhs));
                    a.movss_load_r8_disp(1, c_disp);
                    a.sse_ss_rr(0x59, 0, 1);
                }
                FusedEwStep::DivScalar(lhs, _, false) => {
                    a.movss_load_rsp(0, slot_off(lhs));
                    a.movss_load_r8_disp(1, c_disp);
                    a.sse_ss_rr(0x5E, 0, 1); // divss
                }
                FusedEwStep::Neg(lhs) => {
                    a.movss_load_rsp(0, slot_off(lhs));
                    a.sse_ps_rr(0x57, 0, 3); // xorps
                }
                FusedEwStep::Abs(lhs) => {
                    a.movss_load_rsp(0, slot_off(lhs));
                    a.sse_ps_rr(0x54, 0, 4); // andps
                }
                FusedEwStep::Sqrt(lhs) => {
                    a.movss_load_rsp(0, slot_off(lhs));
                    a.sse_ss_rr(0x51, 0, 0); // sqrtss
                }
                FusedEwStep::Relu(lhs) => {
                    a.movss_load_rsp(0, slot_off(lhs));
                    a.sse_ss_rr(0x5F, 0, 2); // maxss xmm0, +0.0f
                }
                FusedEwStep::StepPos(lhs) => {
                    a.sse_ps_rr(0x28, 0, 2); // movaps xmm0, xmm2 (+0.0f)
                    a.movss_load_rsp(1, slot_off(lhs));
                    a.sse_ss_rr_imm8(0xC2, 0, 1, 0x01); // cmpss xmm0, xmm1, LT
                    a.sse_ps_rr(0x54, 0, 5); // andps xmm0, xmm5 (+1.0f)
                }
                FusedEwStep::Sign(lhs) => {
                    a.sse_ps_rr(0x28, 0, 2); // movaps xmm0, xmm2 (+0.0f)
                    a.movss_load_rsp(1, slot_off(lhs));
                    a.sse_ss_rr_imm8(0xC2, 0, 1, 0x01); // xmm0 = (0.0f < x)
                    a.sse_ps_rr(0x54, 0, 5); // xmm0 = pos
                    a.sse_ss_rr_imm8(0xC2, 1, 2, 0x01); // xmm1 = (x < 0.0f)
                    a.sse_ps_rr(0x54, 1, 5); // xmm1 = neg
                    a.sse_ss_rr(0x5C, 0, 1); // subss xmm0, xmm1
                }
                _ => unreachable!("is_chain_supported pre-filtered unsupported step"),
            }
            a.movss_store_rsp(0, slot_off(dst_slot));
        }

        a.movss_store_r11_rax4(0);
        a.emit(&[0x48, 0x83, 0xC0, 0x01]); // add rax, 1
        a.emit(&[0xE9]); // jmp scalar_loop
        let s_back_patch = a.pos();
        a.emit_i32(0);
        a.patch_rel32(s_back_patch, scalar_loop);

        let scalar_done = a.pos();
        a.patch_rel32(scalar_exit_patch, scalar_done);

        // Epilogue.
        a.emit(&[0x48, 0x89, 0xEC]); // mov rsp, rbp
        a.emit(&[0x5D]); // pop rbp
        a.emit(&[0xC3]); // ret

        (a.buf, consts)
    }

    fn kernel_cache_key(
        n_inputs: usize,
        broadcast_mask: u64,
        steps: &[FusedEwStep],
        use_avx2: bool,
    ) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mix = |h: &mut u64, v: u64| {
            *h ^= v;
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        };
        mix(&mut h, n_inputs as u64);
        mix(&mut h, broadcast_mask);
        mix(&mut h, u64::from(use_avx2));
        mix(&mut h, steps.len() as u64);
        for s in steps {
            match *s {
                FusedEwStep::Add(a, b) => {
                    mix(&mut h, 1);
                    mix(&mut h, a as u64);
                    mix(&mut h, b as u64);
                }
                FusedEwStep::Sub(a, b) => {
                    mix(&mut h, 2);
                    mix(&mut h, a as u64);
                    mix(&mut h, b as u64);
                }
                FusedEwStep::Mul(a, b) => {
                    mix(&mut h, 3);
                    mix(&mut h, a as u64);
                    mix(&mut h, b as u64);
                }
                FusedEwStep::AddScalar(a, c) => {
                    mix(&mut h, 4);
                    mix(&mut h, a as u64);
                    mix(&mut h, c.to_bits());
                }
                FusedEwStep::SubScalar(a, c, r) => {
                    mix(&mut h, 5);
                    mix(&mut h, a as u64);
                    mix(&mut h, c.to_bits());
                    mix(&mut h, u64::from(r));
                }
                FusedEwStep::MulScalar(a, c) => {
                    mix(&mut h, 6);
                    mix(&mut h, a as u64);
                    mix(&mut h, c.to_bits());
                }
                FusedEwStep::DivScalar(a, c, r) => {
                    mix(&mut h, 7);
                    mix(&mut h, a as u64);
                    mix(&mut h, c.to_bits());
                    mix(&mut h, u64::from(r));
                }
                FusedEwStep::Neg(a) => {
                    mix(&mut h, 8);
                    mix(&mut h, a as u64);
                }
                FusedEwStep::Abs(a) => {
                    mix(&mut h, 9);
                    mix(&mut h, a as u64);
                }
                FusedEwStep::Sqrt(a) => {
                    mix(&mut h, 10);
                    mix(&mut h, a as u64);
                }
                FusedEwStep::Relu(a) => {
                    mix(&mut h, 11);
                    mix(&mut h, a as u64);
                }
                FusedEwStep::StepPos(a) => {
                    mix(&mut h, 12);
                    mix(&mut h, a as u64);
                }
                FusedEwStep::Sign(a) => {
                    mix(&mut h, 13);
                    mix(&mut h, a as u64);
                }
                _ => mix(&mut h, 0xFF),
            }
        }
        h
    }

    static JIT_CACHE: OnceLock<RwLock<HashMap<u64, Arc<CompiledNativeKernel>>>> = OnceLock::new();
    static JIT_CACHE_F32: OnceLock<RwLock<HashMap<u64, Arc<CompiledNativeKernelF32>>>> =
        OnceLock::new();

    fn get_or_compile_f32(
        n_inputs: usize,
        broadcast_mask: u64,
        steps: &[FusedEwStep],
        use_avx2: bool,
    ) -> MoraResult<Arc<CompiledNativeKernelF32>> {
        let key =
            kernel_cache_key(n_inputs, broadcast_mask, steps, use_avx2) ^ 0xF320_F320_F320_F320;
        let cache = JIT_CACHE_F32.get_or_init(|| RwLock::new(HashMap::new()));
        if let Some(hit) = cache.read().get(&key).cloned() {
            STAT_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(hit);
        }
        let (code_bytes, consts) = emit_kernel_code_f32(n_inputs, broadcast_mask, steps, use_avx2);
        let wx_buf = os_mem::WxCodeBuffer::from_code(&code_bytes)?;
        let kernel = Arc::new(CompiledNativeKernelF32 {
            code: wx_buf,
            consts,
        });
        let mut w = cache.write();
        if let Some(existing) = w.get(&key).cloned() {
            STAT_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(existing);
        }
        STAT_COMPILED_KERNELS.fetch_add(1, Ordering::Relaxed);
        w.insert(key, Arc::clone(&kernel));
        Ok(kernel)
    }

    fn get_or_compile(
        n_inputs: usize,
        broadcast_mask: u64,
        steps: &[FusedEwStep],
        use_avx2: bool,
    ) -> MoraResult<Arc<CompiledNativeKernel>> {
        let key = kernel_cache_key(n_inputs, broadcast_mask, steps, use_avx2);
        let cache = JIT_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
        if let Some(hit) = cache.read().get(&key).cloned() {
            STAT_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(hit);
        }
        let (code_bytes, consts) = emit_kernel_code(n_inputs, broadcast_mask, steps, use_avx2);
        let wx_buf = os_mem::WxCodeBuffer::from_code(&code_bytes)?;
        let kernel = Arc::new(CompiledNativeKernel {
            code: wx_buf,
            consts,
        });
        let mut w = cache.write();
        if let Some(existing) = w.get(&key).cloned() {
            STAT_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(existing);
        }
        STAT_COMPILED_KERNELS.fetch_add(1, Ordering::Relaxed);
        w.insert(key, Arc::clone(&kernel));
        Ok(kernel)
    }

    pub(super) fn try_run(
        input_flats: &[&[f64]],
        _input_shapes: &[&[usize]],
        steps: &[FusedEwStep],
        out_numel: usize,
        dtype: DType,
        out: &mut Vec<f64>,
    ) -> MoraResult<bool> {
        if !is_enabled() || !is_chain_supported(input_flats.len(), steps) || input_flats.len() > 64
        {
            STAT_FALLBACK_DISPATCHES.fetch_add(1, Ordering::Relaxed);
            return Ok(false);
        }
        if out_numel == 0 {
            out.clear();
            STAT_NATIVE_DISPATCHES.fetch_add(1, Ordering::Relaxed);
            return Ok(true);
        }

        // Support contiguous full-length inputs and 1-element scalar broadcast inputs;
        // general strided broadcast shapes fall back cleanly to `simd_kernels`.
        let mut broadcast_mask: u64 = 0;
        let mut in_ptrs: Vec<*const f64> = Vec::with_capacity(input_flats.len());
        for (i, flat) in input_flats.iter().enumerate() {
            if flat.len() == out_numel {
                in_ptrs.push(flat.as_ptr());
            } else if flat.len() == 1 {
                broadcast_mask |= 1u64 << i;
                in_ptrs.push(flat.as_ptr());
            } else {
                STAT_FALLBACK_DISPATCHES.fetch_add(1, Ordering::Relaxed);
                return Ok(false);
            }
        }

        let use_avx2 = std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("avx");

        // G7: Native `8×f32` AVX2 / `1×f32` SSE strict W^X fast path for `DType::F32`!
        if dtype == DType::F32 {
            let kernel = get_or_compile_f32(input_flats.len(), broadcast_mask, steps, use_avx2)?;
            let f32_inputs: Vec<Vec<f32>> = input_flats
                .iter()
                .map(|flat| flat.iter().map(|&v| v as f32).collect())
                .collect();
            let f32_ptrs: Vec<*const f32> = f32_inputs.iter().map(|v| v.as_ptr()).collect();
            let mut out_f32 = vec![0.0_f32; out_numel];
            kernel.invoke(&f32_ptrs, &mut out_f32);
            out.clear();
            out.extend(out_f32.into_iter().map(f64::from));
            STAT_NATIVE_DISPATCHES.fetch_add(1, Ordering::Relaxed);
            STAT_F32_NATIVE_DISPATCHES.fetch_add(1, Ordering::Relaxed);
            return Ok(true);
        }

        let kernel = get_or_compile(input_flats.len(), broadcast_mask, steps, use_avx2)?;

        out.resize(out_numel, 0.0);
        kernel.invoke(&in_ptrs, out.as_mut_slice());

        if dtype != DType::F64 {
            for v in out.iter_mut() {
                *v = quantize_for_dtype(*v, dtype);
            }
        }
        STAT_NATIVE_DISPATCHES.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    }

    // ── P5-2B: Strict W^X AVX2+FMA `4×8` (`f32`) / `4×4` (`f64`) FusedLinearAct Microkernel JIT ──

    #[repr(C)]
    struct GemmMicroArgs<T> {
        a0: *const T,          // 0x00
        a1: *const T,          // 0x08
        a2: *const T,          // 0x10
        a3: *const T,          // 0x18
        b_col: *const T,       // 0x20
        b_stride_bytes: usize, // 0x28
        bias_col: *const T,    // 0x30
        c0: *mut T,            // 0x38
        c1: *mut T,            // 0x40
        c2: *mut T,            // 0x48
        c3: *mut T,            // 0x50
        k: usize,              // 0x58
    }

    const _: () = {
        assert!(std::mem::offset_of!(GemmMicroArgs<f32>, a0) == 0x00);
        assert!(std::mem::offset_of!(GemmMicroArgs<f32>, a1) == 0x08);
        assert!(std::mem::offset_of!(GemmMicroArgs<f32>, a2) == 0x10);
        assert!(std::mem::offset_of!(GemmMicroArgs<f32>, a3) == 0x18);
        assert!(std::mem::offset_of!(GemmMicroArgs<f32>, b_col) == 0x20);
        assert!(std::mem::offset_of!(GemmMicroArgs<f32>, b_stride_bytes) == 0x28);
        assert!(std::mem::offset_of!(GemmMicroArgs<f32>, bias_col) == 0x30);
        assert!(std::mem::offset_of!(GemmMicroArgs<f32>, c0) == 0x38);
        assert!(std::mem::offset_of!(GemmMicroArgs<f32>, c1) == 0x40);
        assert!(std::mem::offset_of!(GemmMicroArgs<f32>, c2) == 0x48);
        assert!(std::mem::offset_of!(GemmMicroArgs<f32>, c3) == 0x50);
        assert!(std::mem::offset_of!(GemmMicroArgs<f32>, k) == 0x58);
    };

    type JitGemmMicroFn = unsafe extern "C" fn(args: *const c_void);

    struct CompiledGemmMicroKernel {
        code: os_mem::WxCodeBuffer,
    }

    impl CompiledGemmMicroKernel {
        fn invoke<T>(&self, args: &GemmMicroArgs<T>) {
            // SAFETY: `self.code` is an immutable `RX` x86-64 AVX2+FMA leaf microkernel
            // honoring C ABI (`rcx` on Win64, `rdi` on SysV), preserving all callee-saved GP
            // registers (`rbp`, `rbx`, `rsi`) and touching only volatile `ymm0..ymm5`, and
            // `args` points to valid non-overlapping `A`, `B`, optional `bias`, and `C` slices
            // of width >= 8 (`f32`) or >= 4 (`f64`).
            unsafe {
                let f: JitGemmMicroFn = std::mem::transmute(self.code.as_ptr());
                f((args as *const GemmMicroArgs<T>) as *const c_void);
            }
        }
    }

    /// Emit strict W^X x86-64 AVX2+FMA register-blocked GEMM+Bias+ReLU microkernel:
    /// - `is_f64 == false`: `4×8` (`rows_4 = true`) or `1×8` (`rows_4 = false`) `f32` via `vfmadd231ps`
    /// - `is_f64 == true`:  `4×4` (`rows_4 = true`) or `1×4` (`rows_4 = false`) `f64` via `vfmadd231pd`
    ///
    /// Uses ONLY volatile `ymm0..ymm5` (zero XMM callee-save spills on Windows x64 and System V).
    fn emit_gemm_microkernel_code(
        is_f64: bool,
        rows_4: bool,
        has_bias: bool,
        apply_relu: bool,
    ) -> Vec<u8> {
        let mut a = X64Asm::new();
        let pp: u8 = if is_f64 { 0x05 } else { 0x04 }; // L=1, pp=1 (66) for f64, pp=0 for f32
        let bcast_op: u8 = if is_f64 { 0x19 } else { 0x18 }; // vbroadcastsd / vbroadcastss
        let fma_vex2: u8 = if is_f64 { 0xDD } else { 0x5D }; // W=1/0, ~vvvv=~4=0xB, L=1, pp=1 (66)
        let elem_step: u8 = if is_f64 { 8 } else { 4 };

        let mov_r64_r11_disp8 = |asm: &mut X64Asm, reg_code: u8, disp: u8| {
            let rex = 0x49 | if reg_code >= 8 { 0x04 } else { 0x00 };
            let modrm = 0x40 | ((reg_code & 7) << 3) | 3; // [r11 + disp8]
            asm.emit(&[rex, 0x8B, modrm, disp]);
        };
        let vxor_zero = |asm: &mut X64Asm, reg: u8| {
            let vex2 = 0x80 | ((!reg & 0x0F) << 3) | pp;
            asm.emit(&[0xC5, vex2, 0x57, 0xC0 | (reg << 3) | reg]);
        };
        let vadd_ymm4 = |asm: &mut X64Asm, dst: u8| {
            let vex2 = 0x80 | ((!dst & 0x0F) << 3) | pp;
            asm.emit(&[0xC5, vex2, 0x58, 0xC0 | (dst << 3) | 4]);
        };
        let vmax_ymm5 = |asm: &mut X64Asm, dst: u8| {
            let vex2 = 0x80 | ((!dst & 0x0F) << 3) | pp;
            asm.emit(&[0xC5, vex2, 0x5F, 0xC0 | (dst << 3) | 5]);
        };
        let vmovu_store_rcx = |asm: &mut X64Asm, src: u8| {
            let vex2 = if is_f64 { 0xFD } else { 0xFC };
            asm.emit(&[0xC5, vex2, 0x11, (src << 3) | 1]);
        };

        // Prologue: preserve callee-saved `rbp`, `rbx`, `rsi` across Win64 & SysV.
        a.emit(&[0x55]); // push rbp
        a.emit(&[0x48, 0x89, 0xE5]); // mov rbp, rsp
        a.emit(&[0x53]); // push rbx
        a.emit(&[0x56]); // push rsi

        // Move single pointer argument (`args`) into `r11`.
        #[cfg(windows)]
        {
            a.emit(&[0x49, 0x89, 0xCB]); // mov r11, rcx
        }
        #[cfg(not(windows))]
        {
            a.emit(&[0x49, 0x89, 0xFB]); // mov r11, rdi
        }

        // Load hot loop registers from `GemmMicroArgs` at `[r11 + disp8]`.
        mov_r64_r11_disp8(&mut a, 0, 0x00); // rax = a0
        if rows_4 {
            mov_r64_r11_disp8(&mut a, 2, 0x08); // rdx = a1
            mov_r64_r11_disp8(&mut a, 8, 0x10); // r8  = a2
            mov_r64_r11_disp8(&mut a, 9, 0x18); // r9  = a3
        }
        mov_r64_r11_disp8(&mut a, 10, 0x20); // r10 = b_col
        mov_r64_r11_disp8(&mut a, 3, 0x28); // rbx = b_stride_bytes
        mov_r64_r11_disp8(&mut a, 6, 0x58); // rsi = k

        // Zero accumulators `ymm0` (and `ymm1..ymm3` for 4-row microkernel).
        vxor_zero(&mut a, 0);
        if rows_4 {
            vxor_zero(&mut a, 1);
            vxor_zero(&mut a, 2);
            vxor_zero(&mut a, 3);
        }

        // Skip K-loop if k == 0.
        a.emit(&[0x48, 0x85, 0xF6]); // test rsi, rsi
        a.emit(&[0x0F, 0x84]); // jz epilogue
        let skip_k_patch = a.pos();
        a.emit_i32(0);

        // K-loop body: 4×8 (f32) or 4×4 (f64) FMA3 rank-1 update reusing loaded `B` vector across 4 rows!
        let k_loop = a.pos();
        // vmovups/vmovupd ymm4, [r10]
        a.emit(&[0xC4, 0xC1, if is_f64 { 0x7D } else { 0x7C }, 0x10, 0x22]);

        // Row 0: vbroadcastss/sd ymm5, [rax]; vfmadd231ps/pd ymm0, ymm4, ymm5
        a.emit(&[0xC4, 0xE2, 0x7D, bcast_op, 0x28]);
        a.emit(&[0xC4, 0xE2, fma_vex2, 0xB8, 0xC5]);

        if rows_4 {
            // Row 1: vbroadcastss/sd ymm5, [rdx]; vfmadd231ps/pd ymm1, ymm4, ymm5
            a.emit(&[0xC4, 0xE2, 0x7D, bcast_op, 0x2A]);
            a.emit(&[0xC4, 0xE2, fma_vex2, 0xB8, 0xCD]);
            // Row 2: vbroadcastss/sd ymm5, [r8]; vfmadd231ps/pd ymm2, ymm4, ymm5
            a.emit(&[0xC4, 0xC2, 0x7D, bcast_op, 0x28]);
            a.emit(&[0xC4, 0xE2, fma_vex2, 0xB8, 0xD5]);
            // Row 3: vbroadcastss/sd ymm5, [r9]; vfmadd231ps/pd ymm3, ymm4, ymm5
            a.emit(&[0xC4, 0xC2, 0x7D, bcast_op, 0x29]);
            a.emit(&[0xC4, 0xE2, fma_vex2, 0xB8, 0xDD]);
        }

        // Advance `A` pointers by `elem_step` and `B` pointer by `b_stride_bytes` (`rbx`).
        a.emit(&[0x48, 0x83, 0xC0, elem_step]); // add rax, elem_step
        if rows_4 {
            a.emit(&[0x48, 0x83, 0xC2, elem_step]); // add rdx, elem_step
            a.emit(&[0x49, 0x83, 0xC0, elem_step]); // add r8, elem_step
            a.emit(&[0x49, 0x83, 0xC1, elem_step]); // add r9, elem_step
        }
        a.emit(&[0x49, 0x01, 0xDA]); // add r10, rbx

        a.emit(&[0x48, 0x83, 0xEE, 0x01]); // sub rsi, 1
        a.emit(&[0x0F, 0x85]); // jnz k_loop
        let k_back_patch = a.pos();
        a.emit_i32(0);
        a.patch_rel32(k_back_patch, k_loop);

        let epilogue = a.pos();
        a.patch_rel32(skip_k_patch, epilogue);

        // Optional fused bias addition in registers.
        if has_bias {
            mov_r64_r11_disp8(&mut a, 1, 0x30); // rcx = bias_col
            a.emit(&[0xC5, if is_f64 { 0xFD } else { 0xFC }, 0x10, 0x21]); // vmovups/pd ymm4, [rcx]
            vadd_ymm4(&mut a, 0);
            if rows_4 {
                vadd_ymm4(&mut a, 1);
                vadd_ymm4(&mut a, 2);
                vadd_ymm4(&mut a, 3);
            }
        }

        // Optional fused ReLU (`max(acc, +0.0)`) in registers.
        if apply_relu {
            vxor_zero(&mut a, 5);
            vmax_ymm5(&mut a, 0);
            if rows_4 {
                vmax_ymm5(&mut a, 1);
                vmax_ymm5(&mut a, 2);
                vmax_ymm5(&mut a, 3);
            }
        }

        // Store finished output tiles `c0` (and `c1..c3`).
        mov_r64_r11_disp8(&mut a, 1, 0x38); // rcx = c0
        vmovu_store_rcx(&mut a, 0);
        if rows_4 {
            mov_r64_r11_disp8(&mut a, 1, 0x40); // rcx = c1
            vmovu_store_rcx(&mut a, 1);
            mov_r64_r11_disp8(&mut a, 1, 0x48); // rcx = c2
            vmovu_store_rcx(&mut a, 2);
            mov_r64_r11_disp8(&mut a, 1, 0x50); // rcx = c3
            vmovu_store_rcx(&mut a, 3);
        }

        a.emit(&[0xC5, 0xF8, 0x77]); // vzeroupper
        a.emit(&[0x5E]); // pop rsi
        a.emit(&[0x5B]); // pop rbx
        a.emit(&[0x5D]); // pop rbp
        a.emit(&[0xC3]); // ret
        a.buf
    }

    static JIT_GEMM_CACHE: OnceLock<RwLock<HashMap<u64, Arc<CompiledGemmMicroKernel>>>> =
        OnceLock::new();

    fn get_or_compile_gemm_micro(
        is_f64: bool,
        rows_4: bool,
        has_bias: bool,
        apply_relu: bool,
    ) -> MoraResult<Arc<CompiledGemmMicroKernel>> {
        let key: u64 = 0x6E33_0000
            | (u64::from(is_f64) << 3)
            | (u64::from(rows_4) << 2)
            | (u64::from(has_bias) << 1)
            | u64::from(apply_relu);
        let cache = JIT_GEMM_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
        if let Some(hit) = cache.read().get(&key).cloned() {
            STAT_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(hit);
        }
        let code_bytes = emit_gemm_microkernel_code(is_f64, rows_4, has_bias, apply_relu);
        let wx_buf = os_mem::WxCodeBuffer::from_code(&code_bytes)?;
        let kernel = Arc::new(CompiledGemmMicroKernel { code: wx_buf });
        let mut w = cache.write();
        if let Some(existing) = w.get(&key).cloned() {
            STAT_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
            return Ok(existing);
        }
        STAT_COMPILED_KERNELS.fetch_add(1, Ordering::Relaxed);
        w.insert(key, Arc::clone(&kernel));
        Ok(kernel)
    }

    #[allow(clippy::too_many_arguments)]
    fn run_gemm_rows_f32(
        x_f32: &[f32],
        w_f32: &[f32],
        bias_f32: Option<&[f32]>,
        out_f32: &mut [f32],
        rows_here: usize,
        k: usize,
        n: usize,
        k4: &CompiledGemmMicroKernel,
        k1: &CompiledGemmMicroKernel,
        apply_relu: bool,
        post_act: Option<FusedAct>,
    ) {
        let b_stride_bytes = n * std::mem::size_of::<f32>();
        let has_bias = bias_f32.is_some();
        let bias_base = bias_f32.map_or(ptr::null(), |b| b.as_ptr());
        let n_vec = (n / 8) * 8;

        let mut r = 0usize;
        while r + 4 <= rows_here {
            let a0 = &x_f32[r * k..];
            let a1 = &x_f32[(r + 1) * k..];
            let a2 = &x_f32[(r + 2) * k..];
            let a3 = &x_f32[(r + 3) * k..];
            let c_base = out_f32.as_mut_ptr();
            let mut j = 0usize;
            while j < n_vec {
                // SAFETY: `r + 3 < rows_here`, `j + 8 <= n`, so all 4 row slices and B/bias/C
                // panels are strictly in-bounds and non-overlapping.
                let args = unsafe {
                    GemmMicroArgs {
                        a0: a0.as_ptr(),
                        a1: a1.as_ptr(),
                        a2: a2.as_ptr(),
                        a3: a3.as_ptr(),
                        b_col: w_f32.as_ptr().add(j),
                        b_stride_bytes,
                        bias_col: if has_bias {
                            bias_base.add(j)
                        } else {
                            ptr::null()
                        },
                        c0: c_base.add(r * n + j),
                        c1: c_base.add((r + 1) * n + j),
                        c2: c_base.add((r + 2) * n + j),
                        c3: c_base.add((r + 3) * n + j),
                        k,
                    }
                };
                k4.invoke(&args);
                j += 8;
            }
            for rr in r..r + 4 {
                let row_a = &x_f32[rr * k..(rr + 1) * k];
                let row_c = &mut out_f32[rr * n..(rr + 1) * n];
                for jj in n_vec..n {
                    let mut acc = 0.0_f32;
                    for (p, &av) in row_a.iter().enumerate() {
                        acc = av.mul_add(w_f32[p * n + jj], acc);
                    }
                    if let Some(bv) = bias_f32 {
                        acc += bv[jj];
                    }
                    if apply_relu && acc < 0.0 {
                        acc = 0.0;
                    }
                    row_c[jj] = acc;
                }
            }
            r += 4;
        }

        while r < rows_here {
            let a0 = &x_f32[r * k..];
            let c_base = out_f32.as_mut_ptr();
            let mut j = 0usize;
            while j < n_vec {
                // SAFETY: `r < rows_here` and `j + 8 <= n`.
                let args = unsafe {
                    GemmMicroArgs {
                        a0: a0.as_ptr(),
                        a1: ptr::null(),
                        a2: ptr::null(),
                        a3: ptr::null(),
                        b_col: w_f32.as_ptr().add(j),
                        b_stride_bytes,
                        bias_col: if has_bias {
                            bias_base.add(j)
                        } else {
                            ptr::null()
                        },
                        c0: c_base.add(r * n + j),
                        c1: ptr::null_mut(),
                        c2: ptr::null_mut(),
                        c3: ptr::null_mut(),
                        k,
                    }
                };
                k1.invoke(&args);
                j += 8;
            }
            let row_a = &x_f32[r * k..(r + 1) * k];
            let row_c = &mut out_f32[r * n..(r + 1) * n];
            for jj in n_vec..n {
                let mut acc = 0.0_f32;
                for (p, &av) in row_a.iter().enumerate() {
                    acc = av.mul_add(w_f32[p * n + jj], acc);
                }
                if let Some(bv) = bias_f32 {
                    acc += bv[jj];
                }
                if apply_relu && acc < 0.0 {
                    acc = 0.0;
                }
                row_c[jj] = acc;
            }
            r += 1;
        }

        if let Some(act) = post_act {
            for v in out_f32.iter_mut() {
                *v = act.eval(f64::from(*v)) as f32;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_gemm_rows_f64(
        x_f64: &[f64],
        w_f64: &[f64],
        bias_f64: Option<&[f64]>,
        out_f64: &mut [f64],
        rows_here: usize,
        k: usize,
        n: usize,
        k4: &CompiledGemmMicroKernel,
        k1: &CompiledGemmMicroKernel,
        apply_relu: bool,
        post_act: Option<FusedAct>,
    ) {
        let b_stride_bytes = n * std::mem::size_of::<f64>();
        let has_bias = bias_f64.is_some();
        let bias_base = bias_f64.map_or(ptr::null(), |b| b.as_ptr());
        let n_vec = (n / 4) * 4;

        let mut r = 0usize;
        while r + 4 <= rows_here {
            let a0 = &x_f64[r * k..];
            let a1 = &x_f64[(r + 1) * k..];
            let a2 = &x_f64[(r + 2) * k..];
            let a3 = &x_f64[(r + 3) * k..];
            let c_base = out_f64.as_mut_ptr();
            let mut j = 0usize;
            while j < n_vec {
                // SAFETY: `r + 3 < rows_here`, `j + 4 <= n`, all slices strictly in-bounds.
                let args = unsafe {
                    GemmMicroArgs {
                        a0: a0.as_ptr(),
                        a1: a1.as_ptr(),
                        a2: a2.as_ptr(),
                        a3: a3.as_ptr(),
                        b_col: w_f64.as_ptr().add(j),
                        b_stride_bytes,
                        bias_col: if has_bias {
                            bias_base.add(j)
                        } else {
                            ptr::null()
                        },
                        c0: c_base.add(r * n + j),
                        c1: c_base.add((r + 1) * n + j),
                        c2: c_base.add((r + 2) * n + j),
                        c3: c_base.add((r + 3) * n + j),
                        k,
                    }
                };
                k4.invoke(&args);
                j += 4;
            }
            for rr in r..r + 4 {
                let row_a = &x_f64[rr * k..(rr + 1) * k];
                let row_c = &mut out_f64[rr * n..(rr + 1) * n];
                for jj in n_vec..n {
                    let mut acc = 0.0_f64;
                    for (p, &av) in row_a.iter().enumerate() {
                        acc = av.mul_add(w_f64[p * n + jj], acc);
                    }
                    if let Some(bv) = bias_f64 {
                        acc += bv[jj];
                    }
                    if apply_relu && acc < 0.0 {
                        acc = 0.0;
                    }
                    row_c[jj] = acc;
                }
            }
            r += 4;
        }

        while r < rows_here {
            let a0 = &x_f64[r * k..];
            let c_base = out_f64.as_mut_ptr();
            let mut j = 0usize;
            while j < n_vec {
                // SAFETY: `r < rows_here`, `j + 4 <= n`.
                let args = unsafe {
                    GemmMicroArgs {
                        a0: a0.as_ptr(),
                        a1: ptr::null(),
                        a2: ptr::null(),
                        a3: ptr::null(),
                        b_col: w_f64.as_ptr().add(j),
                        b_stride_bytes,
                        bias_col: if has_bias {
                            bias_base.add(j)
                        } else {
                            ptr::null()
                        },
                        c0: c_base.add(r * n + j),
                        c1: ptr::null_mut(),
                        c2: ptr::null_mut(),
                        c3: ptr::null_mut(),
                        k,
                    }
                };
                k1.invoke(&args);
                j += 4;
            }
            let row_a = &x_f64[r * k..(r + 1) * k];
            let row_c = &mut out_f64[r * n..(r + 1) * n];
            for jj in n_vec..n {
                let mut acc = 0.0_f64;
                for (p, &av) in row_a.iter().enumerate() {
                    acc = av.mul_add(w_f64[p * n + jj], acc);
                }
                if let Some(bv) = bias_f64 {
                    acc += bv[jj];
                }
                if apply_relu && acc < 0.0 {
                    acc = 0.0;
                }
                row_c[jj] = acc;
            }
            r += 1;
        }

        if let Some(act) = post_act {
            for v in out_f64.iter_mut() {
                *v = act.eval(*v);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn try_run_fused_linear_act(
        x_vals: &[f64],
        w_vals: &[f64],
        bias: Option<(&[f64], &[usize])>,
        act: FusedAct,
        m_total: usize,
        k: usize,
        n: usize,
        dtype: DType,
        out: &mut Vec<f64>,
    ) -> MoraResult<bool> {
        let has_avx2_fma = is_enabled()
            && std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("avx")
            && std::arch::is_x86_feature_detected!("fma");
        if !has_avx2_fma || !matches!(dtype, DType::F32 | DType::F64) {
            STAT_FALLBACK_DISPATCHES.fetch_add(1, Ordering::Relaxed);
            return Ok(false);
        }
        let min_lanes = if dtype == DType::F32 { 8 } else { 4 };
        if m_total == 0 || n < min_lanes {
            STAT_FALLBACK_DISPATCHES.fetch_add(1, Ordering::Relaxed);
            return Ok(false);
        }
        // Only direct 1D `[n]` or `[1, n]` biases (or no bias) enter the fast microkernel path.
        let simple_bias: Option<&[f64]> = match bias {
            None => None,
            Some((bv, bs))
                if (bs.len() == 1 && bs[0] == n) || (bs.len() == 2 && bs[0] == 1 && bs[1] == n) =>
            {
                Some(bv)
            }
            Some(_) => {
                STAT_FALLBACK_DISPATCHES.fetch_add(1, Ordering::Relaxed);
                return Ok(false);
            }
        };

        let is_f64 = dtype == DType::F64;
        let has_bias = simple_bias.is_some();
        let (apply_relu, post_act) = match act {
            FusedAct::None => (false, None),
            FusedAct::Relu => (true, None),
            other => (false, Some(other)),
        };

        let k4 = get_or_compile_gemm_micro(is_f64, true, has_bias, apply_relu)?;
        let k1 = get_or_compile_gemm_micro(is_f64, false, has_bias, apply_relu)?;

        let workers = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(1);
        let flops = m_total.saturating_mul(k).saturating_mul(n);

        if is_f64 {
            out.resize(m_total * n, 0.0);
            if workers <= 1 || m_total < 4 || flops < 131_072 {
                run_gemm_rows_f64(
                    x_vals,
                    w_vals,
                    simple_bias,
                    out,
                    m_total,
                    k,
                    n,
                    &k4,
                    &k1,
                    apply_relu,
                    post_act,
                );
            } else {
                let chunk_rows = m_total.div_ceil(workers).max(4);
                std::thread::scope(|s| {
                    for (ci, out_chunk) in out.chunks_mut(chunk_rows * n).enumerate() {
                        let r0 = ci * chunk_rows;
                        let rows_here = out_chunk.len() / n;
                        let x_chunk = &x_vals[r0 * k..(r0 + rows_here) * k];
                        let k4_ref = &k4;
                        let k1_ref = &k1;
                        s.spawn(move || {
                            run_gemm_rows_f64(
                                x_chunk,
                                w_vals,
                                simple_bias,
                                out_chunk,
                                rows_here,
                                k,
                                n,
                                k4_ref,
                                k1_ref,
                                apply_relu,
                                post_act,
                            );
                        });
                    }
                });
            }
            STAT_NATIVE_DISPATCHES.fetch_add(1, Ordering::Relaxed);
            STAT_GEMM_NATIVE_DISPATCHES.fetch_add(1, Ordering::Relaxed);
            Ok(true)
        } else {
            let x_f32: Vec<f32> = x_vals.iter().map(|&v| v as f32).collect();
            let w_f32: Vec<f32> = w_vals.iter().map(|&v| v as f32).collect();
            let bias_f32: Option<Vec<f32>> =
                simple_bias.map(|b| b.iter().map(|&v| v as f32).collect());
            let mut out_f32 = vec![0.0_f32; m_total * n];
            let bias_slice = bias_f32.as_deref();
            if workers <= 1 || m_total < 4 || flops < 131_072 {
                run_gemm_rows_f32(
                    &x_f32,
                    &w_f32,
                    bias_slice,
                    &mut out_f32,
                    m_total,
                    k,
                    n,
                    &k4,
                    &k1,
                    apply_relu,
                    post_act,
                );
            } else {
                let chunk_rows = m_total.div_ceil(workers).max(4);
                let x_ref = &x_f32;
                let w_ref = &w_f32;
                std::thread::scope(|s| {
                    for (ci, out_chunk) in out_f32.chunks_mut(chunk_rows * n).enumerate() {
                        let r0 = ci * chunk_rows;
                        let rows_here = out_chunk.len() / n;
                        let x_chunk = &x_ref[r0 * k..(r0 + rows_here) * k];
                        let k4_ref = &k4;
                        let k1_ref = &k1;
                        s.spawn(move || {
                            run_gemm_rows_f32(
                                x_chunk, w_ref, bias_slice, out_chunk, rows_here, k, n, k4_ref,
                                k1_ref, apply_relu, post_act,
                            );
                        });
                    }
                });
            }
            out.clear();
            out.reserve(out_f32.len());
            out.extend(out_f32.into_iter().map(f64::from));
            STAT_NATIVE_DISPATCHES.fetch_add(1, Ordering::Relaxed);
            STAT_F32_NATIVE_DISPATCHES.fetch_add(1, Ordering::Relaxed);
            STAT_GEMM_NATIVE_DISPATCHES.fetch_add(1, Ordering::Relaxed);
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::simd_kernels;

    #[test]
    fn native_wx_jit_is_bit_exact_with_simd_kernels_across_lengths_and_ieee754_edges() {
        let steps = vec![
            FusedEwStep::Mul(0, 1),                 // slot 3: x * y
            FusedEwStep::AddScalar(3, 1.25),        // slot 4: x*y + 1.25
            FusedEwStep::Sub(4, 2),                 // slot 5: (x*y + 1.25) - bias (broadcast!)
            FusedEwStep::MulScalar(5, -0.5),        // slot 6: * -0.5
            FusedEwStep::Neg(6),                    // slot 7: negate
            FusedEwStep::Relu(7),                   // slot 8: relu
            FusedEwStep::Abs(8),                    // slot 9: abs
            FusedEwStep::Sqrt(9),                   // slot 10: sqrt
            FusedEwStep::DivScalar(10, 2.0, false), // slot 11: / 2.0
            FusedEwStep::SubScalar(11, 3.0, true),  // slot 12: 3.0 - s11
        ];
        let bias = vec![0.35_f64];

        for len in [0usize, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 63, 64, 65, 257] {
            let x: Vec<f64> = (0..len)
                .map(|i| ((i as f64) * 0.37 - 4.2) * if i % 3 == 0 { -1.0 } else { 1.0 })
                .collect();
            let y: Vec<f64> = (0..len).map(|i| (i as f64) * -0.19 + 2.1).collect();

            let mut out_jit = Vec::new();
            let mut out_ref = Vec::new();
            let ran = try_run_fused_elementwise_into(
                &[&x, &y, &bias],
                &[&[len], &[len], &[1]],
                &steps,
                &[len],
                len,
                DType::F64,
                &mut out_jit,
            )
            .expect("native JIT execution");

            #[cfg(target_arch = "x86_64")]
            if is_enabled() {
                assert!(
                    ran,
                    "x86-64 native JIT must accept supported chain at len={len}"
                );
            }

            if ran {
                simd_kernels::fused_elementwise_into(
                    &[&x, &y, &bias],
                    &[&[len], &[len], &[1]],
                    &steps,
                    &[len],
                    len,
                    DType::F64,
                    &mut out_ref,
                )
                .expect("simd reference");
                assert_eq!(out_jit.len(), out_ref.len());
                for (i, (&j, &r)) in out_jit.iter().zip(out_ref.iter()).enumerate() {
                    assert_eq!(
                        j.to_bits(),
                        r.to_bits(),
                        "bit-exact mismatch at len={len} idx={i}: jit={j} ({:#x}) vs ref={r} ({:#x})",
                        j.to_bits(),
                        r.to_bits()
                    );
                }
            }
        }

        // IEEE-754 edge cases: -0.0, +0.0, NaN, +Inf, -Inf through Relu / Neg / Abs / Sqrt.
        let edge_x = vec![
            -0.0,
            0.0,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -4.0,
            9.0,
        ];
        let edge_steps = vec![
            FusedEwStep::Neg(0),
            FusedEwStep::Relu(1),
            FusedEwStep::Sqrt(2),
            FusedEwStep::Abs(3),
        ];
        let mut out_jit = Vec::new();
        let mut out_ref = Vec::new();
        let ran = try_run_fused_elementwise_into(
            &[&edge_x],
            &[&[edge_x.len()]],
            &edge_steps,
            &[edge_x.len()],
            edge_x.len(),
            DType::F64,
            &mut out_jit,
        )
        .expect("edge JIT");
        if ran {
            simd_kernels::fused_elementwise_into(
                &[&edge_x],
                &[&[edge_x.len()]],
                &edge_steps,
                &[edge_x.len()],
                edge_x.len(),
                DType::F64,
                &mut out_ref,
            )
            .expect("edge ref");
            for (i, (&j, &r)) in out_jit.iter().zip(out_ref.iter()).enumerate() {
                if r.is_nan() {
                    assert!(j.is_nan(), "idx={i}: expected NaN, got {j}");
                } else {
                    assert_eq!(
                        j.to_bits(),
                        r.to_bits(),
                        "IEEE-754 edge bit mismatch at idx={i}: jit={j} vs ref={r}"
                    );
                }
            }
        }

        #[cfg(target_arch = "x86_64")]
        if is_enabled() {
            let st = stats();
            assert!(st.compiled_kernels >= 1);
            assert!(st.cache_hits >= 1);
            assert!(st.native_dispatches >= 16);
        }
    }

    #[test]
    fn native_wx_jit_8xf32_avx2_is_bit_exact_with_f32_reference_across_lengths_and_edges() {
        let steps = vec![
            FusedEwStep::Mul(0, 1),                 // slot 3: x * y
            FusedEwStep::AddScalar(3, 1.25),        // slot 4: x*y + 1.25
            FusedEwStep::Sub(4, 2),                 // slot 5: (x*y + 1.25) - bias (broadcast!)
            FusedEwStep::MulScalar(5, -0.5),        // slot 6: * -0.5
            FusedEwStep::Neg(6),                    // slot 7: negate
            FusedEwStep::Relu(7),                   // slot 8: relu
            FusedEwStep::Abs(8),                    // slot 9: abs
            FusedEwStep::Sqrt(9),                   // slot 10: sqrt
            FusedEwStep::DivScalar(10, 2.0, false), // slot 11: / 2.0
            FusedEwStep::SubScalar(11, 3.0, true),  // slot 12: 3.0 - s11
        ];
        let bias = vec![0.35_f64];

        let eval_f32_ref = |x: f32, y: f32, b: f32| -> f32 {
            let s3 = x * y;
            let s4 = s3 + 1.25_f32;
            let s5 = s4 - b;
            let s6 = s5 * -0.5_f32;
            let s7 = -s6;
            let s8 = if s7 > 0.0_f32 { s7 } else { 0.0_f32 };
            let s9 = s8.abs();
            let s10 = s9.sqrt();
            let s11 = s10 / 2.0_f32;
            3.0_f32 - s11
        };

        let before = stats().f32_native_dispatches;
        for len in [
            0usize, 1, 2, 3, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 257,
        ] {
            let x: Vec<f64> = (0..len)
                .map(|i| (((i as f32) * 0.37 - 4.2) * if i % 3 == 0 { -1.0 } else { 1.0 }) as f64)
                .collect();
            let y: Vec<f64> = (0..len)
                .map(|i| ((i as f32) * -0.19 + 2.1) as f64)
                .collect();

            let mut out_jit = Vec::new();
            let ran = try_run_fused_elementwise_into(
                &[&x, &y, &bias],
                &[&[len], &[len], &[1]],
                &steps,
                &[len],
                len,
                DType::F32,
                &mut out_jit,
            )
            .expect("native f32 JIT execution");

            #[cfg(target_arch = "x86_64")]
            if is_enabled() {
                assert!(
                    ran,
                    "x86-64 native 8×f32 JIT must accept supported chain at len={len}"
                );
            }

            if ran {
                assert_eq!(out_jit.len(), len);
                for i in 0..len {
                    let got = out_jit[i] as f32;
                    let expected = eval_f32_ref(x[i] as f32, y[i] as f32, bias[0] as f32);
                    assert_eq!(
                        got.to_bits(),
                        expected.to_bits(),
                        "8×f32 bit-exact mismatch at len={len} idx={i}: jit={got} ({:#x}) vs ref={expected} ({:#x})",
                        got.to_bits(),
                        expected.to_bits()
                    );
                }
            }
        }

        // IEEE-754 edge cases for `8×f32` AVX2 + `1×f32` SSE tail (len=9 crosses 8+1 boundary!).
        let edge_x: Vec<f64> = vec![
            -0.0_f32,
            0.0_f32,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            -4.0_f32,
            9.0_f32,
            -16.0_f32,
            25.0_f32,
        ]
        .into_iter()
        .map(f64::from)
        .collect();
        let edge_steps = vec![
            FusedEwStep::Neg(0),
            FusedEwStep::Relu(1),
            FusedEwStep::Sqrt(2),
            FusedEwStep::Abs(3),
        ];
        let mut out_jit = Vec::new();
        let ran = try_run_fused_elementwise_into(
            &[&edge_x],
            &[&[edge_x.len()]],
            &edge_steps,
            &[edge_x.len()],
            edge_x.len(),
            DType::F32,
            &mut out_jit,
        )
        .expect("f32 edge JIT");
        if ran {
            for (i, &in_v) in edge_x.iter().enumerate() {
                let x = in_v as f32;
                let neg = -x;
                let relu = if neg > 0.0_f32 { neg } else { 0.0_f32 };
                let expected = relu.sqrt().abs();
                let got = out_jit[i] as f32;
                if expected.is_nan() {
                    assert!(got.is_nan(), "idx={i}: expected NaN, got {got}");
                } else {
                    assert_eq!(
                        got.to_bits(),
                        expected.to_bits(),
                        "f32 IEEE-754 edge mismatch at idx={i}: jit={got} vs ref={expected}"
                    );
                }
            }
            #[cfg(target_arch = "x86_64")]
            if is_enabled() {
                assert!(
                    stats().f32_native_dispatches > before,
                    "expected f32_native_dispatches counter to increment"
                );
            }
        }
    }

    #[test]
    fn native_wx_jit_fused_linear_act_avx2_fma_matches_simd_across_shapes_and_acts() {
        let before_gemm = stats().gemm_native_dispatches;
        let shapes = [
            (1usize, 1usize, 8usize),
            (3, 5, 11),
            (4, 7, 8),
            (5, 13, 17),
            (9, 16, 25),
            (17, 33, 19),
            (4, 0, 8),
        ];
        for dtype in [DType::F32, DType::F64] {
            for act in [
                FusedAct::None,
                FusedAct::Relu,
                FusedAct::Gelu,
                FusedAct::Silu,
            ] {
                for use_bias in [false, true] {
                    for &(m, k, n) in &shapes {
                        let x: Vec<f64> = (0..m * k)
                            .map(|i| (((i * 7 + 3) % 19) as f64 - 9.0) * 0.125)
                            .collect();
                        let w: Vec<f64> = (0..k * n)
                            .map(|i| (((i * 5 + 1) % 17) as f64 - 8.0) * 0.125)
                            .collect();
                        let b: Vec<f64> = (0..n).map(|j| (j as f64) * 0.05 - 0.3).collect();
                        let bias_opt = use_bias.then_some((b.as_slice(), std::slice::from_ref(&n)));

                        let mut out_jit = Vec::new();
                        let ran = try_run_fused_linear_act_into(
                            &x,
                            &w,
                            bias_opt,
                            act,
                            m,
                            k,
                            n,
                            dtype,
                            &mut out_jit,
                        )
                        .expect("native GEMM JIT");

                        let mut out_ref = Vec::new();
                        simd_kernels::fused_linear_act_into(
                            &x,
                            &w,
                            bias_opt,
                            act,
                            m,
                            k,
                            n,
                            &[m, n],
                            dtype,
                            &mut out_ref,
                        );

                        if ran {
                            assert_eq!(out_jit.len(), out_ref.len());
                            let tol = if dtype == DType::F32 { 2e-4 } else { 1e-11 };
                            for (i, (&j, &r)) in out_jit.iter().zip(out_ref.iter()).enumerate() {
                                assert!(
                                    (j - r).abs() <= tol,
                                    "GEMM JIT mismatch dtype={dtype:?} act={act:?} bias={use_bias} shape=({m},{k},{n}) idx={i}: jit={j} vs ref={r}"
                                );
                            }
                        }
                    }
                }
            }
        }
        #[cfg(target_arch = "x86_64")]
        if is_enabled()
            && std::arch::is_x86_feature_detected!("avx2")
            && std::arch::is_x86_feature_detected!("fma")
        {
            assert!(
                stats().gemm_native_dispatches > before_gemm,
                "expected gemm_native_dispatches to increment on AVX2+FMA CPU"
            );
        }
    }

    #[test]
    fn native_wx_jit_step_pos_and_sign_avx2_sse2_f64_f32_exact() {
        // 13 elements: exercises AVX2 vector loop (4×f64 / 8×f32) AND SSE2 scalar tail + IEEE-754 edges!
        let x: Vec<f64> = vec![
            -3.5,
            -0.001,
            -0.0,
            0.0,
            0.001,
            4.25,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NAN,
            -1.0,
            1.0,
            -7.0,
            2.5,
        ];
        let g: Vec<f64> = vec![
            2.0, -1.5, 3.0, 4.0, -2.5, 0.5, 1.0, -1.0, 5.0, 6.0, -3.0, 0.25, 8.0,
        ];
        // Chain: `g * StepPos(x) + Sign(x)` — exercises both P6-2 backward activation ops!
        let steps = vec![
            FusedEwStep::StepPos(0),
            FusedEwStep::Mul(1, 2),
            FusedEwStep::Sign(0),
            FusedEwStep::Add(3, 4),
        ];
        for dtype in [DType::F64, DType::F32] {
            let before = stats().native_dispatches;
            let mut out_jit = Vec::new();
            let ran = try_run_fused_elementwise_into(
                &[&x, &g],
                &[&[x.len()], &[g.len()]],
                &steps,
                &[x.len()],
                x.len(),
                dtype,
                &mut out_jit,
            )
            .expect("StepPos/Sign JIT");
            let mut out_ref = Vec::new();
            simd_kernels::fused_elementwise_into(
                &[&x, &g],
                &[&[x.len()], &[g.len()]],
                &steps,
                &[x.len()],
                x.len(),
                dtype,
                &mut out_ref,
            )
            .expect("StepPos/Sign SIMD ref");
            assert!(ran, "StepPos/Sign chain must be JIT-eligible on x86-64");
            assert_eq!(out_jit.len(), out_ref.len());
            for (i, (&jv, &rv)) in out_jit.iter().zip(out_ref.iter()).enumerate() {
                assert_eq!(
                    jv.to_bits(),
                    rv.to_bits(),
                    "StepPos/Sign JIT mismatch dtype={dtype:?} idx={i}: jit={jv} vs ref={rv}"
                );
            }
            #[cfg(target_arch = "x86_64")]
            if is_enabled() {
                assert!(stats().native_dispatches > before);
            }
        }
    }
}
