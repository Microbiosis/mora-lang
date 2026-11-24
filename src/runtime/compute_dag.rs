//! v0.110.0: Native `TensorComputeDag`, Deep Operator Fusion, Hardware-SIMD CPU Backend,
//! Compile-Time Buffer Lifetime Reuse (`DagMemoryPlan`), and Structural Graph Cache (`DagPlanCache`).
//!
//! Architecture:
//! - Explicit inspectable compute-graph IR (`DagNode` / `DagOp`) captured from arbitrary Mora
//!   functions (including higher-order `grad(f)` backward passes!) via `trace_dag(f, inputs...)`.
//! - Four-way operator fusion (`TensorComputeDag::fuse`):
//!   1. `FusedFlashAttention`: fuses `Softmax((Q @ K^T) * scale [+ mask]) @ V` (absorbing trailing
//!      `Transpose` on `K`) into a tiled FlashAttention-2 online-softmax kernel (`Br=32, Bc=32`,
//!      zero `O(Sq * Sk)` attention score matrix allocation).
//!   2. `FusedRmsNormLinear`: fuses `RMSNorm(x, eps) [* gamma] @ W [+ bias] [+ Act]` for 2D and
//!      batched ND `[..., M, K] @ [K, N]` into one cache-local tiled pass with zero `[..., K]` heap buffer.
//!   3. `FusedLinearAct`: fuses `(X @ W) [+ bias] [+ Act]` for 2D and batched ND inputs.
//!   4. `FusedElementwise`: fuses maximal single-consumer elementwise subgraphs into one cache-local
//!      multi-threaded loop, followed by dead-node elimination (DCE) & index compaction.
//! - Compile-time linear-scan liveness analysis (`DagMemoryPlan`) assigns non-overlapping intermediate
//!   tensors to a minimal pool of reusable physical scratch buffers (`DagWorkspace`).
//! - Thread-safe structural compilation cache (`DagPlanCache`) hosted on [`crate::runtime::EngineShared`]
//!   caches fused plans, memory plans, constant staging buffers, and reusable workspaces.
//! - Kernels run on a [`DeviceBackend`]: the host CPU SIMD backend, a live hardware GPU backend
//!   (`CUDA` / `Vulkan` / `Metal`) when a driver is present, or the host-emulated backend otherwise.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::common::sync::{Mutex, RwLock};

use crate::common::{DType, Device};
use crate::error::{MoraError, MoraResult};
use crate::runtime::autograd::{
    AutogradTape, TapeOp, scatter_add_flat, sum_axis_flat, transpose_flat,
};
use crate::runtime::simd_kernels;
use crate::value::tensor::{AlignedByteBuffer, Tensor, checked_numel, quantize_for_dtype};

/// Live VRAM residency, transfer, and native low-precision dispatch counters for a [`DeviceBackend`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VramTelemetry {
    pub live_buffers: u64,
    pub live_bytes: u64,
    pub h2d_copies: u64,
    pub d2h_copies: u64,
    pub vram_hits: u64,
    pub native_f16_dispatches: u64,
    pub native_bf16_dispatches: u64,
}

/// Owned GPU VRAM allocation (`CUdeviceptr`, `VkBuffer + VkDeviceMemory`, or `MTLBuffer`)
/// with deterministic RAII release back to the owning hardware device/arena on [`Drop`].
pub trait DeviceVramBuffer: Send + Sync + std::fmt::Debug {
    fn device(&self) -> &Device;
    fn byte_len(&self) -> usize;
    /// Underlying driver buffer handle (`CUdeviceptr`, `VkBuffer`, or `MTLBuffer` address).
    fn raw_handle(&self) -> u64;
    /// Upload exact hardware-packed little-endian bytes (`src.len() == self.byte_len()`).
    fn upload_bytes(&self, src: &[u8]) -> MoraResult<()>;
    /// Download exact hardware-packed little-endian bytes (`dst.len() == self.byte_len()`).
    fn download_bytes(&self, dst: &mut [u8]) -> MoraResult<()>;
}

/// Activation fused into `DagOp::FusedLinearAct` and `DagOp::FusedRmsNormLinear`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FusedAct {
    None,
    Relu,
    Gelu,
    Silu,
    Tanh,
    Sigmoid,
}

impl FusedAct {
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "linear",
            Self::Relu => "relu",
            Self::Gelu => "gelu",
            Self::Silu => "silu",
            Self::Tanh => "tanh",
            Self::Sigmoid => "sigmoid",
        }
    }

    #[inline]
    pub fn eval(self, x: f64) -> f64 {
        match self {
            Self::None => x,
            Self::Relu => {
                if x > 0.0 {
                    x
                } else {
                    0.0
                }
            }
            Self::Gelu => {
                let c = (2.0_f64 / std::f64::consts::PI).sqrt();
                0.5 * x * (1.0 + (c * (x + 0.044715 * x.powi(3))).tanh())
            }
            Self::Silu => x / (1.0 + (-x).exp()),
            Self::Tanh => x.tanh(),
            Self::Sigmoid => 1.0 / (1.0 + (-x).exp()),
        }
    }
}

/// One micro-operation inside a fused elementwise chain (`DagOp::FusedElementwise`).
/// Operands reference either external graph inputs (`0..n_inputs`) or prior steps
/// (`n_inputs + step_idx`).
#[derive(Debug, Clone, PartialEq)]
pub enum FusedEwStep {
    Add(usize, usize),
    Sub(usize, usize),
    Mul(usize, usize),
    Div(usize, usize),
    Pow(usize, usize),
    AddScalar(usize, f64),
    SubScalar(usize, f64, bool),
    MulScalar(usize, f64),
    DivScalar(usize, f64, bool),
    PowScalar(usize, f64, bool),
    Neg(usize),
    Relu(usize),
    Gelu(usize),
    Silu(usize),
    Tanh(usize),
    Sigmoid(usize),
    Exp(usize),
    Ln(usize),
    Sqrt(usize),
    Sin(usize),
    Cos(usize),
    Abs(usize),
}

/// Abstract hardware execution backend for [`TensorComputeDag`] kernels.
pub trait DeviceBackend: Send + Sync {
    fn kind(&self) -> Device;
    fn name(&self) -> &str;
    fn simd_isa(&self) -> &'static str;
    fn worker_threads(&self) -> usize;
    fn is_available(&self) -> bool;

    fn run_matmul_into(
        &self,
        a: &[f64],
        b: &[f64],
        a_shape: &[usize],
        b_shape: &[usize],
        dtype: DType,
        out: &mut Vec<f64>,
    ) -> MoraResult<()>;

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
    ) -> MoraResult<()>;

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
    ) -> MoraResult<()>;

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
    ) -> MoraResult<()>;

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
    ) -> MoraResult<()>;

    /// Whether this backend owns physical GPU VRAM supporting resident [`DeviceVramBuffer`]s.
    fn supports_vram_residency(&self) -> bool {
        false
    }

    /// Allocate a physical GPU VRAM buffer of `byte_len` bytes on this backend (`None` on CPU/emulated).
    fn alloc_vram_buffer(&self, _byte_len: usize) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        Ok(None)
    }

    /// Snapshot live VRAM allocation, H2D/D2H copy, cache-hit, and native f16/bf16 dispatch counters.
    fn vram_telemetry(&self) -> VramTelemetry {
        VramTelemetry::default()
    }

    /// Zero-copy VRAM-to-VRAM MatMul kernel operating directly on resident [`DeviceVramBuffer`]s.
    /// When `dst` is `Some(buf)` on the same device with sufficient byte capacity, writes in-place
    /// into `buf` without allocating a new VRAM buffer.
    fn run_matmul_vram(
        &self,
        _a: &dyn DeviceVramBuffer,
        _b: &dyn DeviceVramBuffer,
        _a_shape: &[usize],
        _b_shape: &[usize],
        _dtype: DType,
        _dst: Option<Arc<dyn DeviceVramBuffer>>,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        Ok(None)
    }

    /// Zero-copy VRAM-to-VRAM `FusedLinearAct` kernel operating directly on resident buffers.
    #[allow(clippy::too_many_arguments)]
    fn run_fused_linear_act_vram(
        &self,
        _x: &dyn DeviceVramBuffer,
        _w: &dyn DeviceVramBuffer,
        _bias: Option<(&dyn DeviceVramBuffer, &[usize])>,
        _act: FusedAct,
        _m_total: usize,
        _k: usize,
        _n: usize,
        _out_shape: &[usize],
        _dtype: DType,
        _dst: Option<Arc<dyn DeviceVramBuffer>>,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        Ok(None)
    }

    /// Zero-copy VRAM-to-VRAM `FusedRmsNormLinear` kernel operating directly on resident buffers.
    #[allow(clippy::too_many_arguments)]
    fn run_fused_rms_norm_linear_vram(
        &self,
        _x: &dyn DeviceVramBuffer,
        _eps: f64,
        _gamma: Option<(&dyn DeviceVramBuffer, &[usize])>,
        _w: &dyn DeviceVramBuffer,
        _bias: Option<(&dyn DeviceVramBuffer, &[usize])>,
        _act: FusedAct,
        _m_total: usize,
        _k: usize,
        _n: usize,
        _x_shape: &[usize],
        _out_shape: &[usize],
        _dtype: DType,
        _dst: Option<Arc<dyn DeviceVramBuffer>>,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        Ok(None)
    }

    /// Zero-copy VRAM-to-VRAM `FusedFlashAttention` kernel operating directly on resident buffers.
    #[allow(clippy::too_many_arguments)]
    fn run_fused_flash_attention_vram(
        &self,
        _q: &dyn DeviceVramBuffer,
        _k: &dyn DeviceVramBuffer,
        _v: &dyn DeviceVramBuffer,
        _mask: Option<(&dyn DeviceVramBuffer, &[usize])>,
        _q_shape: &[usize],
        _k_shape: &[usize],
        _v_shape: &[usize],
        _out_shape: &[usize],
        _scale: f64,
        _k_is_transposed: bool,
        _dtype: DType,
        _dst: Option<Arc<dyn DeviceVramBuffer>>,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        Ok(None)
    }

    /// Zero-copy VRAM-to-VRAM `FusedElementwise` kernel operating directly on resident buffers.
    #[allow(clippy::too_many_arguments)]
    fn run_fused_elementwise_vram(
        &self,
        _inputs: &[&dyn DeviceVramBuffer],
        _input_shapes: &[&[usize]],
        _steps: &[FusedEwStep],
        _out_shape: &[usize],
        _out_numel: usize,
        _dtype: DType,
        _dst: Option<Arc<dyn DeviceVramBuffer>>,
    ) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        Ok(None)
    }

    /// P4-1B: Compile (cached) and launch a user-defined source-level GPU `kernel` directly on
    /// persistent GPU VRAM buffers (`Ok(true)` = executed on GPU hardware; `Ok(false)` = fall back to CPU).
    fn launch_custom_kernel(
        &self,
        _kernel: &crate::common::kernel_ir::KernelProgram,
        _grid: u32,
        _block: u32,
        _args: &[crate::runtime::gpu::kernel_compiler::KernelLaunchArg],
    ) -> MoraResult<bool> {
        Ok(false)
    }
}

/// Native host CPU backend executing multi-threaded cache-blocked SIMD fused kernels.
#[derive(Debug, Default)]
pub struct CpuDeviceBackend;

impl DeviceBackend for CpuDeviceBackend {
    fn kind(&self) -> Device {
        Device::Cpu
    }

    fn name(&self) -> &str {
        "cpu"
    }

    fn simd_isa(&self) -> &'static str {
        simd_kernels::simd_isa_name()
    }

    fn worker_threads(&self) -> usize {
        simd_kernels::effective_worker_threads()
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
        simd_kernels::matmul_simd_into(a, b, a_shape, b_shape, out)?;
        if dtype != DType::F64 {
            for v in out.iter_mut() {
                *v = quantize_for_dtype(*v, dtype);
            }
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
        simd_kernels::fused_linear_act_into(x, w, bias, act, m_total, k, n, out_shape, dtype, out);
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
        simd_kernels::fused_rms_norm_linear_into(
            x, eps, gamma, w, bias, act, m_total, k, n, x_shape, out_shape, dtype, out,
        );
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
        if crate::runtime::native_jit::try_run_fused_elementwise_into(
            input_flats,
            input_shapes,
            steps,
            out_shape,
            out_numel,
            dtype,
            out,
        )? {
            return Ok(());
        }
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

/// Host-emulated accelerator backend for `cuda:N` / `metal:N` tagged compute graphs.
#[derive(Debug)]
pub struct EmulatedDeviceBackend {
    device: Device,
    cpu: CpuDeviceBackend,
}

impl DeviceBackend for EmulatedDeviceBackend {
    fn kind(&self) -> Device {
        self.device.clone()
    }

    fn name(&self) -> &str {
        match &self.device {
            Device::Cpu => "cpu",
            Device::Cuda(_) => "cuda(host-emulated)",
            Device::Metal(_) => "metal(host-emulated)",
            Device::Vulkan(_) => "vulkan(host-emulated)",
        }
    }

    fn simd_isa(&self) -> &'static str {
        self.cpu.simd_isa()
    }

    fn worker_threads(&self) -> usize {
        self.cpu.worker_threads()
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
        self.cpu.run_matmul_into(a, b, a_shape, b_shape, dtype, out)
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
        self.cpu
            .run_fused_linear_act_into(x, w, bias, act, m_total, k, n, out_shape, dtype, out)
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
        self.cpu.run_fused_rms_norm_linear_into(
            x, eps, gamma, w, bias, act, m_total, k, n, x_shape, out_shape, dtype, out,
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
        self.cpu.run_fused_flash_attention_into(
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
        self.cpu.run_fused_elementwise_into(
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

/// Resolve the [`DeviceBackend`] responsible for executing kernels on `device`.
/// Prefers live hardware GPU drivers (`CUDA` / `Metal` / `Vulkan`) when detected at runtime,
/// falling back cleanly to `EmulatedDeviceBackend` on CPU-only hosts or when
/// `MORA_FORCE_CPU_EMULATION=1` is set.
pub fn backend_for_device(device: &Device) -> Arc<dyn DeviceBackend> {
    match device {
        Device::Cpu => Arc::new(CpuDeviceBackend),
        other => {
            if let Some(hw) = crate::runtime::gpu::resolve_hardware_backend(other) {
                hw
            } else {
                Arc::new(EmulatedDeviceBackend {
                    device: other.clone(),
                    cpu: CpuDeviceBackend,
                })
            }
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 2. Compute Graph IR (`DagOp`, `DagNode`, `TensorComputeDag`)
// ════════════════════════════════════════════════════════════════════════════

/// Node operation inside a [`TensorComputeDag`].
#[derive(Debug, Clone, PartialEq)]
pub enum DagOp {
    Input(usize),
    ConstTensor(Arc<Tensor>),
    Add(usize, usize),
    Sub(usize, usize),
    Mul(usize, usize),
    Div(usize, usize),
    Pow(usize, usize),
    AddScalar(usize, f64),
    SubScalar(usize, f64, bool),
    MulScalar(usize, f64),
    DivScalar(usize, f64, bool),
    PowScalar(usize, f64, bool),
    Neg(usize),
    MatMul(usize, usize),
    Sum(usize),
    Mean(usize),
    SumAxis(usize, usize, bool),
    Reshape(usize, Vec<usize>),
    Transpose(usize, usize, usize),
    Identity(usize),
    Gather(usize, Vec<usize>),
    ScatterAdd(usize, usize, Vec<usize>),
    Relu(usize),
    Gelu(usize),
    Silu(usize),
    Tanh(usize),
    Sigmoid(usize),
    Exp(usize),
    Ln(usize),
    Sqrt(usize),
    Sin(usize),
    Cos(usize),
    Abs(usize),
    /// Exact Heaviside step (`1.0` if `x > 0.0`, else `0.0`) used by IR-level symbolic AD of `Relu`.
    StepPos(usize),
    /// Exact signum (`1.0` if `x > 0.0`, `-1.0` if `x < 0.0`, else `0.0`) used by IR-level AD of `Abs`.
    Sign(usize),
    Softmax(usize),
    RmsNorm(usize, f64),
    LayerNorm(usize, f64),
    FusedLinearAct {
        x: usize,
        w: usize,
        bias: Option<usize>,
        act: FusedAct,
    },
    FusedRmsNormLinear {
        x: usize,
        eps: f64,
        gamma: Option<usize>,
        w: usize,
        bias: Option<usize>,
        act: FusedAct,
    },
    FusedFlashAttention {
        q: usize,
        k: usize,
        v: usize,
        mask: Option<usize>,
        scale: f64,
        k_is_transposed: bool,
    },
    FusedElementwise {
        inputs: Vec<usize>,
        steps: Vec<FusedEwStep>,
    },
}

impl DagOp {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Input(_) => "Input",
            Self::ConstTensor(_) => "ConstTensor",
            Self::Add(_, _) => "Add",
            Self::Sub(_, _) => "Sub",
            Self::Mul(_, _) => "Mul",
            Self::Div(_, _) => "Div",
            Self::Pow(_, _) => "Pow",
            Self::AddScalar(_, _) => "AddScalar",
            Self::SubScalar(_, _, _) => "SubScalar",
            Self::MulScalar(_, _) => "MulScalar",
            Self::DivScalar(_, _, _) => "DivScalar",
            Self::PowScalar(_, _, _) => "PowScalar",
            Self::Neg(_) => "Neg",
            Self::MatMul(_, _) => "MatMul",
            Self::Sum(_) => "Sum",
            Self::Mean(_) => "Mean",
            Self::SumAxis(_, _, _) => "SumAxis",
            Self::Reshape(_, _) => "Reshape",
            Self::Transpose(_, _, _) => "Transpose",
            Self::Identity(_) => "Identity",
            Self::Gather(_, _) => "Gather",
            Self::ScatterAdd(_, _, _) => "ScatterAdd",
            Self::Relu(_) => "Relu",
            Self::Gelu(_) => "Gelu",
            Self::Silu(_) => "Silu",
            Self::Tanh(_) => "Tanh",
            Self::Sigmoid(_) => "Sigmoid",
            Self::Exp(_) => "Exp",
            Self::Ln(_) => "Ln",
            Self::Sqrt(_) => "Sqrt",
            Self::Sin(_) => "Sin",
            Self::Cos(_) => "Cos",
            Self::Abs(_) => "Abs",
            Self::StepPos(_) => "StepPos",
            Self::Sign(_) => "Sign",
            Self::Softmax(_) => "Softmax",
            Self::RmsNorm(_, _) => "RmsNorm",
            Self::LayerNorm(_, _) => "LayerNorm",
            Self::FusedLinearAct { .. } => "FusedLinearAct",
            Self::FusedRmsNormLinear { .. } => "FusedRmsNormLinear",
            Self::FusedFlashAttention { .. } => "FusedFlashAttention",
            Self::FusedElementwise { .. } => "FusedElementwise",
        }
    }

    pub fn is_fused(&self) -> bool {
        matches!(
            self,
            Self::FusedLinearAct { .. }
                | Self::FusedRmsNormLinear { .. }
                | Self::FusedFlashAttention { .. }
                | Self::FusedElementwise { .. }
        )
    }

    pub fn inputs(&self) -> Vec<usize> {
        match self {
            Self::Input(_) | Self::ConstTensor(_) => Vec::new(),
            Self::Add(a, b)
            | Self::Sub(a, b)
            | Self::Mul(a, b)
            | Self::Div(a, b)
            | Self::Pow(a, b)
            | Self::MatMul(a, b) => vec![*a, *b],
            Self::AddScalar(a, _)
            | Self::SubScalar(a, _, _)
            | Self::MulScalar(a, _)
            | Self::DivScalar(a, _, _)
            | Self::PowScalar(a, _, _)
            | Self::Neg(a)
            | Self::Sum(a)
            | Self::Mean(a)
            | Self::SumAxis(a, _, _)
            | Self::Reshape(a, _)
            | Self::Transpose(a, _, _)
            | Self::Identity(a)
            | Self::Gather(a, _)
            | Self::ScatterAdd(a, _, _)
            | Self::Relu(a)
            | Self::Gelu(a)
            | Self::Silu(a)
            | Self::Tanh(a)
            | Self::Sigmoid(a)
            | Self::Exp(a)
            | Self::Ln(a)
            | Self::Sqrt(a)
            | Self::Sin(a)
            | Self::Cos(a)
            | Self::Abs(a)
            | Self::StepPos(a)
            | Self::Sign(a)
            | Self::Softmax(a)
            | Self::RmsNorm(a, _)
            | Self::LayerNorm(a, _) => vec![*a],
            Self::FusedLinearAct { x, w, bias, .. } => {
                let mut v = vec![*x, *w];
                if let Some(b) = bias {
                    v.push(*b);
                }
                v
            }
            Self::FusedRmsNormLinear {
                x, gamma, w, bias, ..
            } => {
                let mut v = vec![*x];
                if let Some(g) = gamma {
                    v.push(*g);
                }
                v.push(*w);
                if let Some(b) = bias {
                    v.push(*b);
                }
                v
            }
            Self::FusedFlashAttention { q, k, v, mask, .. } => {
                let mut out = vec![*q, *k, *v];
                if let Some(m) = mask {
                    out.push(*m);
                }
                out
            }
            Self::FusedElementwise { inputs, .. } => inputs.clone(),
        }
    }

    fn remap_inputs(&mut self, map: &[usize]) {
        let m = |idx: &mut usize| *idx = map[*idx];
        match self {
            Self::Input(_) | Self::ConstTensor(_) => {}
            Self::Add(a, b)
            | Self::Sub(a, b)
            | Self::Mul(a, b)
            | Self::Div(a, b)
            | Self::Pow(a, b)
            | Self::MatMul(a, b) => {
                m(a);
                m(b);
            }
            Self::AddScalar(a, _)
            | Self::SubScalar(a, _, _)
            | Self::MulScalar(a, _)
            | Self::DivScalar(a, _, _)
            | Self::PowScalar(a, _, _)
            | Self::Neg(a)
            | Self::Sum(a)
            | Self::Mean(a)
            | Self::SumAxis(a, _, _)
            | Self::Reshape(a, _)
            | Self::Transpose(a, _, _)
            | Self::Identity(a)
            | Self::Gather(a, _)
            | Self::ScatterAdd(a, _, _)
            | Self::Relu(a)
            | Self::Gelu(a)
            | Self::Silu(a)
            | Self::Tanh(a)
            | Self::Sigmoid(a)
            | Self::Exp(a)
            | Self::Ln(a)
            | Self::Sqrt(a)
            | Self::Sin(a)
            | Self::Cos(a)
            | Self::Abs(a)
            | Self::StepPos(a)
            | Self::Sign(a)
            | Self::Softmax(a)
            | Self::RmsNorm(a, _)
            | Self::LayerNorm(a, _) => m(a),
            Self::FusedLinearAct { x, w, bias, .. } => {
                m(x);
                m(w);
                if let Some(b) = bias {
                    m(b);
                }
            }
            Self::FusedRmsNormLinear {
                x, gamma, w, bias, ..
            } => {
                m(x);
                if let Some(g) = gamma {
                    m(g);
                }
                m(w);
                if let Some(b) = bias {
                    m(b);
                }
            }
            Self::FusedFlashAttention { q, k, v, mask, .. } => {
                m(q);
                m(k);
                m(v);
                if let Some(mk) = mask {
                    m(mk);
                }
            }
            Self::FusedElementwise { inputs, .. } => {
                for i in inputs {
                    m(i);
                }
            }
        }
    }
}

/// One node in a [`TensorComputeDag`].
#[derive(Debug, Clone, PartialEq)]
pub struct DagNode {
    pub op: DagOp,
    pub shape: Vec<usize>,
    pub dtype: DType,
    pub device: Device,
}

// ════════════════════════════════════════════════════════════════════════════
// 3. Static Buffer Lifetime Reuse (`DagMemoryPlan`) & Workspace Arena
// ════════════════════════════════════════════════════════════════════════════

/// One physical reusable scratch buffer slot in a [`DagMemoryPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferSlotPlan {
    pub id: usize,
    pub capacity_elems: usize,
    pub dtype: DType,
    pub device: Device,
    pub bytes: usize,
}

/// Compile-time linear-scan liveness & scratch buffer reuse plan for a [`TensorComputeDag`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagMemoryPlan {
    /// Per-node scratch slot assignment (`None` for `Input`, `ConstTensor`, and final `output`).
    pub node_slots: Vec<Option<usize>>,
    /// Minimal physical scratch buffer pool allocated once and reused across non-overlapping nodes.
    pub slots: Vec<BufferSlotPlan>,
    pub total_intermediates: usize,
    pub buffer_slots: usize,
    pub naive_peak_bytes: usize,
    pub planned_peak_bytes: usize,
    pub reuse_count: usize,
    /// Number of physical GPU VRAM scratch slots in `slots` (`device != Cpu`).
    pub vram_peak_slots: usize,
    /// Sum of planned physical GPU VRAM scratch slot bytes (`device != Cpu`).
    pub vram_peak_bytes: usize,
}

impl DagMemoryPlan {
    /// Perform linear-scan liveness analysis and best-fit scratch buffer assignment for `dag`.
    pub fn build(dag: &TensorComputeDag) -> Self {
        let n = dag.nodes.len();
        let mut last_use = vec![0usize; n];
        for (idx, node) in dag.nodes.iter().enumerate() {
            last_use[idx] = idx;
            for pred in node.op.inputs() {
                if idx > last_use[pred] {
                    last_use[pred] = idx;
                }
            }
        }
        if dag.output < n {
            last_use[dag.output] = n;
        }
        // Zero-copy view ops (`Reshape` / `Identity`) may alias their input VRAM buffer;
        // extend source liveness through any alias chain so physical VRAM slots never overwrite live views.
        for idx in (0..n).rev() {
            if let DagOp::Reshape(src, _) | DagOp::Identity(src) = dag.nodes[idx].op
                && last_use[idx] > last_use[src]
            {
                last_use[src] = last_use[idx];
            }
        }

        let mut node_slots: Vec<Option<usize>> = vec![None; n];
        let mut slots: Vec<BufferSlotPlan> = Vec::new();
        // `slot_live_until[slot_id] = last_use_idx` of the current occupant (reusable once `idx > slot_live_until`).
        let mut slot_live_until: Vec<usize> = Vec::new();
        let mut slot_uses: Vec<usize> = Vec::new();

        let mut total_intermediates = 0usize;
        let mut naive_peak_bytes = 0usize;

        for (idx, node) in dag.nodes.iter().enumerate() {
            if idx == dag.output || matches!(node.op, DagOp::Input(_) | DagOp::ConstTensor(_)) {
                continue;
            }
            total_intermediates += 1;
            let numel = checked_numel(&node.shape).unwrap_or(1);
            let bytes = node.dtype.storage_bytes(numel);
            naive_peak_bytes += bytes;

            // Find best-fit reusable dead slot (`slot_live_until[s] < idx` guarantees zero aliasing with `idx`'s inputs!).
            let mut best_fit: Option<(usize, usize)> = None;
            let mut grow_candidate: Option<(usize, usize)> = None;
            for (s_id, slot) in slots.iter().enumerate() {
                if slot_live_until[s_id] >= idx
                    || slot.dtype != node.dtype
                    || slot.device != node.device
                {
                    continue;
                }
                if slot.capacity_elems >= numel {
                    let waste = slot.capacity_elems - numel;
                    if best_fit.is_none_or(|(_, best_waste)| waste < best_waste) {
                        best_fit = Some((s_id, waste));
                    }
                } else {
                    let deficit = numel - slot.capacity_elems;
                    if grow_candidate.is_none_or(|(_, best_def)| deficit < best_def) {
                        grow_candidate = Some((s_id, deficit));
                    }
                }
            }

            let chosen_slot = if let Some((s_id, _)) = best_fit {
                s_id
            } else if let Some((s_id, _)) = grow_candidate {
                slots[s_id].capacity_elems = numel;
                slots[s_id].bytes = node.dtype.storage_bytes(numel);
                s_id
            } else {
                let s_id = slots.len();
                slots.push(BufferSlotPlan {
                    id: s_id,
                    capacity_elems: numel,
                    dtype: node.dtype,
                    device: node.device.clone(),
                    bytes,
                });
                slot_live_until.push(0);
                slot_uses.push(0);
                s_id
            };

            slot_live_until[chosen_slot] = last_use[idx];
            slot_uses[chosen_slot] += 1;
            node_slots[idx] = Some(chosen_slot);
        }

        let planned_peak_bytes: usize = slots.iter().map(|s| s.bytes).sum();
        let reuse_count: usize = slot_uses.iter().map(|&c| c.saturating_sub(1)).sum();
        let buffer_slots = slots.len();
        let vram_peak_slots = slots.iter().filter(|s| s.device != Device::Cpu).count();
        let vram_peak_bytes: usize = slots
            .iter()
            .filter(|s| s.device != Device::Cpu)
            .map(|s| s.bytes)
            .sum();

        Self {
            node_slots,
            slots,
            total_intermediates,
            buffer_slots,
            naive_peak_bytes,
            planned_peak_bytes,
            reuse_count,
            vram_peak_slots,
            vram_peak_bytes,
        }
    }
}

/// Reusable physical scratch arena matching a [`DagMemoryPlan`] (both host CPU and GPU VRAM slots).
#[derive(Debug)]
struct DagWorkspace {
    slots: Vec<Vec<f64>>,
    /// Persistent physical GPU VRAM scratch buffers keyed by `DagMemoryPlan` slot id.
    /// Allocated lazily once per slot at `slot.bytes` capacity and reused across non-overlapping
    /// nodes within each run AND across repeated `CompiledDagPlan::execute` calls.
    vram_slots: Vec<Option<Arc<dyn DeviceVramBuffer>>>,
    output_buf: Vec<f64>,
}

impl DagWorkspace {
    fn from_plan(plan: &DagMemoryPlan, output_numel: usize) -> Self {
        let slots = plan
            .slots
            .iter()
            .map(|s| Vec::with_capacity(s.capacity_elems))
            .collect();
        let vram_slots = vec![None; plan.slots.len()];
        Self {
            slots,
            vram_slots,
            output_buf: Vec::with_capacity(output_numel),
        }
    }
}

/// Compiled execution plan containing a fused DAG, static buffer reuse plan, pre-decoded constants,
/// and a thread-safe checkoutable reusable [`DagWorkspace`] (host + physical GPU VRAM scratch pool).
#[derive(Debug)]
pub struct CompiledDagPlan {
    pub fingerprint: u64,
    pub dag: TensorComputeDag,
    pub memory_plan: DagMemoryPlan,
    const_flats: Vec<Option<Vec<f64>>>,
    output_numel: usize,
    workspace: Mutex<Option<DagWorkspace>>,
    vram_scratch_allocs: AtomicU64,
    vram_buffer_reuses: AtomicU64,
}

impl CompiledDagPlan {
    pub(crate) fn compile(dag: &TensorComputeDag) -> Self {
        let fused_dag = if dag.fused { dag.clone() } else { dag.fuse() };
        let fingerprint = fused_dag.fingerprint();
        let memory_plan = DagMemoryPlan::build(&fused_dag);
        let const_flats: Vec<Option<Vec<f64>>> = fused_dag
            .nodes
            .iter()
            .map(|n| match &n.op {
                DagOp::ConstTensor(t) => Some(t.to_flat_vec()),
                _ => None,
            })
            .collect();
        let output_numel = fused_dag
            .nodes
            .get(fused_dag.output)
            .and_then(|n| checked_numel(&n.shape).ok())
            .unwrap_or(1);
        let ws = DagWorkspace::from_plan(&memory_plan, output_numel);
        Self {
            fingerprint,
            dag: fused_dag,
            memory_plan,
            const_flats,
            output_numel,
            workspace: Mutex::new(Some(ws)),
            vram_scratch_allocs: AtomicU64::new(0),
            vram_buffer_reuses: AtomicU64::new(0),
        }
    }

    /// Snapshot `(vram_scratch_allocs, vram_buffer_reuses)` observed across executions of this plan.
    pub(crate) fn vram_reuse_stats(&self) -> (u64, u64) {
        (
            self.vram_scratch_allocs.load(Ordering::Relaxed),
            self.vram_buffer_reuses.load(Ordering::Relaxed),
        )
    }

    /// Execute this compiled plan using its pooled reusable [`DagWorkspace`] (falling back to a
    /// private workspace if another `spawn`ed task is concurrently executing the exact same plan).
    pub(crate) fn execute(&self, inputs: &[Arc<Tensor>]) -> MoraResult<Arc<Tensor>> {
        let mut ws = self
            .workspace
            .try_lock()
            .and_then(|mut guard| guard.take())
            .unwrap_or_else(|| DagWorkspace::from_plan(&self.memory_plan, self.output_numel));
        let res = execute_dag_with_workspace(
            &self.dag,
            &self.memory_plan,
            &self.const_flats,
            &mut ws,
            inputs,
            Some((&self.vram_scratch_allocs, &self.vram_buffer_reuses)),
        );
        if let Some(mut guard) = self.workspace.try_lock()
            && guard.is_none()
        {
            *guard = Some(ws);
        }
        res
    }
}

/// Snapshot statistics for [`DagPlanCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DagCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: usize,
}

/// Thread-safe structural compilation cache owned by [`crate::runtime::EngineShared`].
#[derive(Debug)]
pub struct DagPlanCache {
    entries: RwLock<HashMap<u64, Arc<CompiledDagPlan>>>,
    max_entries: usize,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl Default for DagPlanCache {
    fn default() -> Self {
        Self::new(256)
    }
}

impl DagPlanCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            max_entries: max_entries.max(16),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn stats(&self) -> DagCacheStats {
        DagCacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            entries: self.entries.read().len(),
        }
    }

    /// Look up or compile & cache a [`CompiledDagPlan`] keyed by structural DAG fingerprint.
    pub fn get_or_compile(&self, dag: &TensorComputeDag) -> Arc<CompiledDagPlan> {
        let key = dag.fingerprint();
        if let Some(plan) = self.entries.read().get(&key)
            && plan.dag.input_specs == dag.input_specs
        {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return Arc::clone(plan);
        }

        let plan = Arc::new(CompiledDagPlan::compile(dag));
        let fused_key = plan.fingerprint;
        self.misses.fetch_add(1, Ordering::Relaxed);

        let mut map = self.entries.write();
        if map.len() >= self.max_entries {
            map.clear();
        }
        map.insert(key, Arc::clone(&plan));
        if fused_key != key {
            map.insert(fused_key, Arc::clone(&plan));
        }
        plan
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 4. `TensorComputeDag` Construction, Fingerprinting, 4-Pass Fusion, Execution
// ════════════════════════════════════════════════════════════════════════════

/// Static inspectable compute graph captured from Mora tensor operations.
#[derive(Debug, Clone)]
pub struct TensorComputeDag {
    pub input_specs: Vec<(Vec<usize>, DType, Device)>,
    pub nodes: Vec<DagNode>,
    pub output: usize,
    pub fused: bool,
    /// P4-3: Number of DType-safe algebraic / layout simplifications applied during optimization.
    pub(crate) algebraic_rewrites: usize,
    compiled_plan: Option<Arc<CompiledDagPlan>>,
}

/// P4-3: Per-node roofline cost analysis entry produced by [`DagCostModel`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DagNodeCost {
    pub(crate) node_id: usize,
    pub(crate) op: &'static str,
    pub(crate) flops: u64,
    pub(crate) bytes_read: u64,
    pub(crate) bytes_written: u64,
    pub(crate) arithmetic_intensity: f64,
    pub(crate) bound: &'static str,
}

/// P4-3: Whole-DAG roofline cost, bandwidth savings, algebraic rewrite, and native JIT report.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DagCostReport {
    pub(crate) node_costs: Vec<DagNodeCost>,
    pub(crate) total_flops: u64,
    pub(crate) total_bytes_read: u64,
    pub(crate) total_bytes_written: u64,
    pub(crate) total_memory_bytes: u64,
    pub(crate) arithmetic_intensity: f64,
    pub(crate) bottleneck: &'static str,
    pub(crate) fusion_bytes_saved: u64,
    pub(crate) fused_nodes: usize,
    pub(crate) total_nodes: usize,
    pub(crate) algebraic_rewrites: usize,
    pub(crate) native_jit_eligible_nodes: usize,
    pub(crate) native_jit_isa: &'static str,
    pub(crate) native_jit_compiled: u64,
    pub(crate) native_jit_cache_hits: u64,
    pub(crate) native_jit_dispatches: u64,
    pub(crate) native_jit_f32_dispatches: u64,
    pub(crate) vram_scratch_allocs: u64,
    pub(crate) vram_buffer_reuses: u64,
}

/// P4-3: Analytical FLOPs, memory-traffic, and roofline cost model for [`TensorComputeDag`].
pub(crate) struct DagCostModel;

impl DagCostModel {
    fn ew_step_flops(step: &FusedEwStep) -> u64 {
        match step {
            FusedEwStep::Add(_, _)
            | FusedEwStep::Sub(_, _)
            | FusedEwStep::Mul(_, _)
            | FusedEwStep::Div(_, _)
            | FusedEwStep::AddScalar(_, _)
            | FusedEwStep::SubScalar(_, _, _)
            | FusedEwStep::MulScalar(_, _)
            | FusedEwStep::DivScalar(_, _, _)
            | FusedEwStep::Neg(_)
            | FusedEwStep::Relu(_)
            | FusedEwStep::Abs(_) => 1,
            FusedEwStep::Sqrt(_)
            | FusedEwStep::Exp(_)
            | FusedEwStep::Ln(_)
            | FusedEwStep::Sin(_)
            | FusedEwStep::Cos(_)
            | FusedEwStep::Tanh(_) => 4,
            FusedEwStep::Sigmoid(_) => 5,
            FusedEwStep::Silu(_) => 6,
            FusedEwStep::Gelu(_) | FusedEwStep::Pow(_, _) | FusedEwStep::PowScalar(_, _, _) => 8,
        }
    }

    fn act_flops(act: FusedAct) -> u64 {
        match act {
            FusedAct::None => 0,
            FusedAct::Relu => 1,
            FusedAct::Tanh => 4,
            FusedAct::Sigmoid => 5,
            FusedAct::Silu => 6,
            FusedAct::Gelu => 8,
        }
    }

    fn classify(flops: u64, mem_bytes: u64, dev: &Device) -> (f64, &'static str) {
        if flops == 0 && mem_bytes == 0 {
            return (0.0, "zero-cost");
        }
        let ai = if mem_bytes > 0 {
            flops as f64 / mem_bytes as f64
        } else {
            f64::INFINITY
        };
        let ridge = match dev {
            Device::Cpu => 8.0,
            Device::Cuda(_) | Device::Vulkan(_) | Device::Metal(_) => 32.0,
        };
        if ai >= ridge {
            (ai, "compute-bound")
        } else {
            (ai, "memory-bound")
        }
    }

    pub(crate) fn analyze(dag: &TensorComputeDag) -> DagCostReport {
        let mut node_costs = Vec::with_capacity(dag.nodes.len());
        let mut total_flops: u64 = 0;
        let mut total_bytes_read: u64 = 0;
        let mut total_bytes_written: u64 = 0;
        let mut fusion_bytes_saved: u64 = 0;
        let mut fused_nodes: usize = 0;
        let mut native_jit_eligible_nodes: usize = 0;

        let node_bytes = |idx: usize| -> u64 {
            let n = &dag.nodes[idx];
            let elems = checked_numel(&n.shape).unwrap_or(1);
            n.dtype.storage_bytes(elems) as u64
        };

        for (idx, node) in dag.nodes.iter().enumerate() {
            let out_elems = checked_numel(&node.shape).unwrap_or(1) as u64;
            let out_bytes = node.dtype.storage_bytes(out_elems as usize) as u64;
            if node.op.is_fused() {
                fused_nodes += 1;
            }
            let (flops, read_b, write_b, saved_b) = match &node.op {
                DagOp::Input(_)
                | DagOp::ConstTensor(_)
                | DagOp::Reshape(_, _)
                | DagOp::Identity(_) => (0, 0, 0, 0),
                DagOp::Transpose(a, _, _) | DagOp::Gather(a, _) | DagOp::ScatterAdd(a, _, _) => {
                    (0, node_bytes(*a), out_bytes, 0)
                }
                DagOp::Add(a, b) | DagOp::Sub(a, b) | DagOp::Mul(a, b) | DagOp::Div(a, b) => {
                    (out_elems, node_bytes(*a) + node_bytes(*b), out_bytes, 0)
                }
                DagOp::Pow(a, b) => (out_elems * 8, node_bytes(*a) + node_bytes(*b), out_bytes, 0),
                DagOp::AddScalar(a, _)
                | DagOp::SubScalar(a, _, _)
                | DagOp::MulScalar(a, _)
                | DagOp::DivScalar(a, _, _)
                | DagOp::Neg(a)
                | DagOp::Relu(a)
                | DagOp::Abs(a)
                | DagOp::StepPos(a)
                | DagOp::Sign(a) => (out_elems, node_bytes(*a), out_bytes, 0),
                DagOp::PowScalar(a, _, _) | DagOp::Gelu(a) => {
                    (out_elems * 8, node_bytes(*a), out_bytes, 0)
                }
                DagOp::Sqrt(a)
                | DagOp::Exp(a)
                | DagOp::Ln(a)
                | DagOp::Sin(a)
                | DagOp::Cos(a)
                | DagOp::Tanh(a) => (out_elems * 4, node_bytes(*a), out_bytes, 0),
                DagOp::Sigmoid(a) => (out_elems * 5, node_bytes(*a), out_bytes, 0),
                DagOp::Silu(a) => (out_elems * 6, node_bytes(*a), out_bytes, 0),
                DagOp::Sum(a) | DagOp::Mean(a) | DagOp::SumAxis(a, _, _) => {
                    let in_elems = checked_numel(&dag.nodes[*a].shape).unwrap_or(1) as u64;
                    (in_elems, node_bytes(*a), out_bytes, 0)
                }
                DagOp::Softmax(a) => (out_elems * 5, node_bytes(*a), out_bytes, 0),
                DagOp::RmsNorm(a, _) => (out_elems * 6, node_bytes(*a), out_bytes, 0),
                DagOp::LayerNorm(a, _) => (out_elems * 8, node_bytes(*a), out_bytes, 0),
                DagOp::MatMul(a, b) => {
                    let k = dag.nodes[*a].shape.last().copied().unwrap_or(1) as u64;
                    let flops = out_elems.saturating_mul(2).saturating_mul(k);
                    (flops, node_bytes(*a) + node_bytes(*b), out_bytes, 0)
                }
                DagOp::FusedLinearAct { x, w, bias, act } => {
                    let k = dag.nodes[*x].shape.last().copied().unwrap_or(1) as u64;
                    let mut flops = out_elems.saturating_mul(2).saturating_mul(k);
                    let mut read_b = node_bytes(*x) + node_bytes(*w);
                    let mut saved = 0u64;
                    if let Some(b) = bias {
                        flops = flops.saturating_add(out_elems);
                        read_b += node_bytes(*b);
                        saved += out_bytes * 2;
                    }
                    if *act != FusedAct::None {
                        flops = flops.saturating_add(out_elems * Self::act_flops(*act));
                        saved += out_bytes * 2;
                    }
                    (flops, read_b, out_bytes, saved)
                }
                DagOp::FusedRmsNormLinear {
                    x,
                    gamma,
                    w,
                    bias,
                    act,
                    ..
                } => {
                    let x_elems = checked_numel(&dag.nodes[*x].shape).unwrap_or(1) as u64;
                    let x_bytes = node_bytes(*x);
                    let k = dag.nodes[*x].shape.last().copied().unwrap_or(1) as u64;
                    let mut flops = x_elems.saturating_mul(4);
                    let mut read_b = x_bytes + node_bytes(*w);
                    let mut saved = x_bytes * 2;
                    if let Some(g) = gamma {
                        flops = flops.saturating_add(x_elems);
                        read_b += node_bytes(*g);
                        saved += x_bytes * 2;
                    }
                    flops = flops.saturating_add(out_elems.saturating_mul(2).saturating_mul(k));
                    if let Some(b) = bias {
                        flops = flops.saturating_add(out_elems);
                        read_b += node_bytes(*b);
                        saved += out_bytes * 2;
                    }
                    if *act != FusedAct::None {
                        flops = flops.saturating_add(out_elems * Self::act_flops(*act));
                        saved += out_bytes * 2;
                    }
                    (flops, read_b, out_bytes, saved)
                }
                DagOp::FusedFlashAttention {
                    q,
                    k,
                    v,
                    mask,
                    k_is_transposed,
                    ..
                } => {
                    let q_sh = &dag.nodes[*q].shape;
                    let k_sh = &dag.nodes[*k].shape;
                    let v_sh = &dag.nodes[*v].shape;
                    let qr = q_sh.len();
                    let kr = k_sh.len();
                    let vr = v_sh.len();
                    let batches: u64 = q_sh[..qr.saturating_sub(2)]
                        .iter()
                        .map(|&d| d as u64)
                        .product::<u64>()
                        .max(1);
                    let s_q = q_sh.get(qr.saturating_sub(2)).copied().unwrap_or(1) as u64;
                    let d_k = q_sh.get(qr.saturating_sub(1)).copied().unwrap_or(1) as u64;
                    let s_k = if *k_is_transposed {
                        k_sh.get(kr.saturating_sub(1)).copied().unwrap_or(1) as u64
                    } else {
                        k_sh.get(kr.saturating_sub(2)).copied().unwrap_or(1) as u64
                    };
                    let d_v = v_sh.get(vr.saturating_sub(1)).copied().unwrap_or(1) as u64;
                    let score_elems = batches.saturating_mul(s_q).saturating_mul(s_k);
                    let qk_flops = score_elems.saturating_mul(2).saturating_mul(d_k);
                    let sm_flops = score_elems.saturating_mul(5);
                    let pv_flops = score_elems.saturating_mul(2).saturating_mul(d_v);
                    let mut read_b = node_bytes(*q) + node_bytes(*k) + node_bytes(*v);
                    if let Some(m) = mask {
                        read_b += node_bytes(*m);
                    }
                    let score_bytes = node.dtype.storage_bytes(score_elems as usize) as u64;
                    // Eliminates writing+reading Q@K^T score matrix and Softmax probs matrix!
                    let saved = score_bytes.saturating_mul(4);
                    (qk_flops + sm_flops + pv_flops, read_b, out_bytes, saved)
                }
                DagOp::FusedElementwise { inputs, steps } => {
                    if node.device == Device::Cpu
                        && crate::runtime::native_jit::is_chain_supported(inputs.len(), steps)
                    {
                        native_jit_eligible_nodes += 1;
                    }
                    let step_flops: u64 = steps.iter().map(Self::ew_step_flops).sum();
                    let read_b: u64 = inputs.iter().map(|&i| node_bytes(i)).sum();
                    let saved = (steps.len().saturating_sub(1) as u64)
                        .saturating_mul(out_bytes)
                        .saturating_mul(2);
                    (
                        out_elems.saturating_mul(step_flops),
                        read_b,
                        out_bytes,
                        saved,
                    )
                }
            };

            let mem = read_b + write_b;
            let (ai, bound) = Self::classify(flops, mem, &node.device);
            total_flops = total_flops.saturating_add(flops);
            total_bytes_read = total_bytes_read.saturating_add(read_b);
            total_bytes_written = total_bytes_written.saturating_add(write_b);
            fusion_bytes_saved = fusion_bytes_saved.saturating_add(saved_b);
            node_costs.push(DagNodeCost {
                node_id: idx,
                op: node.op.name(),
                flops,
                bytes_read: read_b,
                bytes_written: write_b,
                arithmetic_intensity: ai,
                bound,
            });
        }

        let total_memory_bytes = total_bytes_read + total_bytes_written;
        let out_dev = dag.output_device();
        let (arithmetic_intensity, bottleneck) =
            Self::classify(total_flops, total_memory_bytes, &out_dev);
        let jit_stats = crate::runtime::native_jit::stats();
        let (vram_scratch_allocs, vram_buffer_reuses) = dag
            .compiled_plan
            .as_ref()
            .map_or((0, 0), |p| p.vram_reuse_stats());

        DagCostReport {
            node_costs,
            total_flops,
            total_bytes_read,
            total_bytes_written,
            total_memory_bytes,
            arithmetic_intensity,
            bottleneck,
            fusion_bytes_saved,
            fused_nodes,
            total_nodes: dag.nodes.len(),
            algebraic_rewrites: dag.algebraic_rewrites,
            native_jit_eligible_nodes,
            native_jit_isa: jit_stats.isa,
            native_jit_compiled: jit_stats.compiled_kernels,
            native_jit_cache_hits: jit_stats.cache_hits,
            native_jit_dispatches: jit_stats.native_dispatches,
            native_jit_f32_dispatches: jit_stats.f32_native_dispatches,
            vram_scratch_allocs,
            vram_buffer_reuses,
        }
    }
}

impl PartialEq for TensorComputeDag {
    fn eq(&self, other: &Self) -> bool {
        self.input_specs == other.input_specs
            && self.nodes == other.nodes
            && self.output == other.output
            && self.fused == other.fused
    }
}

impl TensorComputeDag {
    /// Convert a traced [`AutogradTape`] ending at `out_idx` into a [`TensorComputeDag`].
    pub fn from_tape(
        tape: &AutogradTape,
        out_idx: usize,
        leaf_indices: &[usize],
    ) -> MoraResult<Self> {
        let tape_nodes = tape.nodes();
        if out_idx >= tape_nodes.len() {
            return Err(MoraError::Runtime(format!(
                "trace_dag: output node {out_idx} out of bounds"
            )));
        }

        let mut leaf_slot_map: HashMap<usize, usize> = HashMap::new();
        let mut input_specs = Vec::with_capacity(leaf_indices.len());
        for (slot, &li) in leaf_indices.iter().enumerate() {
            let tn = tape_nodes.get(li).ok_or_else(|| {
                MoraError::Runtime(format!("trace_dag: leaf node {li} out of bounds"))
            })?;
            leaf_slot_map.insert(li, slot);
            input_specs.push((tn.shape.clone(), tn.dtype, tn.device.clone()));
        }

        let mut nodes: Vec<DagNode> = Vec::new();
        let mut tape_to_dag: Vec<Option<usize>> = vec![None; out_idx + 1];

        fn push_const_node(
            nodes: &mut Vec<DagNode>,
            vals: &[f64],
            shape: &[usize],
            dt: DType,
            dev: &Device,
        ) -> MoraResult<usize> {
            let t = Tensor::from_raw(vals.to_vec(), shape.to_vec(), dt, dev.clone(), false)?;
            let id = nodes.len();
            nodes.push(DagNode {
                op: DagOp::ConstTensor(t),
                shape: shape.to_vec(),
                dtype: dt,
                device: dev.clone(),
            });
            Ok(id)
        }

        for (t_idx, tn) in tape_nodes[..=out_idx].iter().enumerate() {
            let resolve_opt = |opt: Option<usize>,
                               const_vals: Option<&[f64]>,
                               shape: &[usize],
                               nodes_ref: &mut Vec<DagNode>,
                               map_ref: &[Option<usize>]|
             -> MoraResult<usize> {
                if let Some(ti) = opt {
                    map_ref[ti].ok_or_else(|| {
                        MoraError::Runtime(format!("trace_dag: missing dependency node {ti}"))
                    })
                } else {
                    let vals = const_vals.ok_or_else(|| {
                        MoraError::Runtime("trace_dag: missing constant tensor payload".to_string())
                    })?;
                    push_const_node(nodes_ref, vals, shape, tn.dtype, &tn.device)
                }
            };

            let op = match &tn.op {
                TapeOp::Leaf => {
                    let slot = leaf_slot_map.get(&t_idx).copied().ok_or_else(|| {
                        MoraError::Runtime(format!("trace_dag: untracked leaf {t_idx} encountered"))
                    })?;
                    DagOp::Input(slot)
                }
                TapeOp::Add {
                    a,
                    b,
                    a_const,
                    b_const,
                    a_shape,
                    b_shape,
                } => {
                    let da =
                        resolve_opt(*a, a_const.as_deref(), a_shape, &mut nodes, &tape_to_dag)?;
                    let db =
                        resolve_opt(*b, b_const.as_deref(), b_shape, &mut nodes, &tape_to_dag)?;
                    DagOp::Add(da, db)
                }
                TapeOp::Sub {
                    a,
                    b,
                    a_const,
                    b_const,
                    a_shape,
                    b_shape,
                } => {
                    let da =
                        resolve_opt(*a, a_const.as_deref(), a_shape, &mut nodes, &tape_to_dag)?;
                    let db =
                        resolve_opt(*b, b_const.as_deref(), b_shape, &mut nodes, &tape_to_dag)?;
                    DagOp::Sub(da, db)
                }
                TapeOp::Mul {
                    a,
                    b,
                    a_vals,
                    b_vals,
                    a_shape,
                    b_shape,
                } => {
                    let da = match *a {
                        Some(ti) => tape_to_dag[ti].expect("mapped"),
                        None => push_const_node(&mut nodes, a_vals, a_shape, tn.dtype, &tn.device)?,
                    };
                    let db = match *b {
                        Some(ti) => tape_to_dag[ti].expect("mapped"),
                        None => push_const_node(&mut nodes, b_vals, b_shape, tn.dtype, &tn.device)?,
                    };
                    DagOp::Mul(da, db)
                }
                TapeOp::Div {
                    a,
                    b,
                    a_vals,
                    b_vals,
                    a_shape,
                    b_shape,
                } => {
                    let da = match *a {
                        Some(ti) => tape_to_dag[ti].expect("mapped"),
                        None => push_const_node(&mut nodes, a_vals, a_shape, tn.dtype, &tn.device)?,
                    };
                    let db = match *b {
                        Some(ti) => tape_to_dag[ti].expect("mapped"),
                        None => push_const_node(&mut nodes, b_vals, b_shape, tn.dtype, &tn.device)?,
                    };
                    DagOp::Div(da, db)
                }
                TapeOp::Pow {
                    a,
                    b,
                    a_vals,
                    b_vals,
                    a_shape,
                    b_shape,
                    ..
                } => {
                    let da = match *a {
                        Some(ti) => tape_to_dag[ti].expect("mapped"),
                        None => push_const_node(&mut nodes, a_vals, a_shape, tn.dtype, &tn.device)?,
                    };
                    let db = match *b {
                        Some(ti) => tape_to_dag[ti].expect("mapped"),
                        None => push_const_node(&mut nodes, b_vals, b_shape, tn.dtype, &tn.device)?,
                    };
                    DagOp::Pow(da, db)
                }
                TapeOp::AddScalar { a, scalar } => {
                    DagOp::AddScalar(tape_to_dag[*a].expect("mapped"), *scalar)
                }
                TapeOp::SubScalar {
                    a,
                    scalar,
                    reversed,
                } => DagOp::SubScalar(tape_to_dag[*a].expect("mapped"), *scalar, *reversed),
                TapeOp::MulScalar { a, scalar } => {
                    DagOp::MulScalar(tape_to_dag[*a].expect("mapped"), *scalar)
                }
                TapeOp::DivScalar {
                    a,
                    scalar,
                    reversed,
                    ..
                } => DagOp::DivScalar(tape_to_dag[*a].expect("mapped"), *scalar, *reversed),
                TapeOp::PowScalar {
                    a,
                    scalar,
                    reversed,
                    ..
                } => DagOp::PowScalar(tape_to_dag[*a].expect("mapped"), *scalar, *reversed),
                TapeOp::Neg { a } => DagOp::Neg(tape_to_dag[*a].expect("mapped")),
                TapeOp::MatMul {
                    a,
                    b,
                    a_vals,
                    b_vals,
                    a_shape,
                    b_shape,
                } => {
                    let da = match *a {
                        Some(ti) => tape_to_dag[ti].expect("mapped"),
                        None => push_const_node(&mut nodes, a_vals, a_shape, tn.dtype, &tn.device)?,
                    };
                    let db = match *b {
                        Some(ti) => tape_to_dag[ti].expect("mapped"),
                        None => push_const_node(&mut nodes, b_vals, b_shape, tn.dtype, &tn.device)?,
                    };
                    DagOp::MatMul(da, db)
                }
                TapeOp::Sum { a, .. } => DagOp::Sum(tape_to_dag[*a].expect("mapped")),
                TapeOp::Mean { a, .. } => DagOp::Mean(tape_to_dag[*a].expect("mapped")),
                TapeOp::SumAxis {
                    a, axis, keepdim, ..
                } => DagOp::SumAxis(tape_to_dag[*a].expect("mapped"), *axis, *keepdim),
                TapeOp::Reshape { a, .. } => {
                    DagOp::Reshape(tape_to_dag[*a].expect("mapped"), tn.shape.clone())
                }
                TapeOp::Transpose { a, dim0, dim1, .. } => {
                    DagOp::Transpose(tape_to_dag[*a].expect("mapped"), *dim0, *dim1)
                }
                TapeOp::Identity { a } => DagOp::Identity(tape_to_dag[*a].expect("mapped")),
                TapeOp::Gather {
                    a,
                    src_flat_indices,
                    ..
                } => DagOp::Gather(tape_to_dag[*a].expect("mapped"), src_flat_indices.clone()),
                TapeOp::ScatterAdd {
                    a,
                    out_shape,
                    dst_flat_indices,
                } => {
                    let out_n = checked_numel(out_shape)?;
                    DagOp::ScatterAdd(
                        tape_to_dag[*a].expect("mapped"),
                        out_n,
                        dst_flat_indices.clone(),
                    )
                }
                TapeOp::Relu { a, .. } => DagOp::Relu(tape_to_dag[*a].expect("mapped")),
                TapeOp::Gelu { a, .. } => DagOp::Gelu(tape_to_dag[*a].expect("mapped")),
                TapeOp::Silu { a, .. } => DagOp::Silu(tape_to_dag[*a].expect("mapped")),
                TapeOp::Tanh { a, .. } => DagOp::Tanh(tape_to_dag[*a].expect("mapped")),
                TapeOp::Sigmoid { a, .. } => DagOp::Sigmoid(tape_to_dag[*a].expect("mapped")),
                TapeOp::Exp { a, .. } => DagOp::Exp(tape_to_dag[*a].expect("mapped")),
                TapeOp::Ln { a, .. } => DagOp::Ln(tape_to_dag[*a].expect("mapped")),
                TapeOp::Sqrt { a, .. } => DagOp::Sqrt(tape_to_dag[*a].expect("mapped")),
                TapeOp::Sin { a, .. } => DagOp::Sin(tape_to_dag[*a].expect("mapped")),
                TapeOp::Cos { a, .. } => DagOp::Cos(tape_to_dag[*a].expect("mapped")),
                TapeOp::Abs { a, .. } => DagOp::Abs(tape_to_dag[*a].expect("mapped")),
                TapeOp::Softmax { a, .. } => DagOp::Softmax(tape_to_dag[*a].expect("mapped")),
                TapeOp::RmsNorm { a, eps, .. } => {
                    DagOp::RmsNorm(tape_to_dag[*a].expect("mapped"), *eps)
                }
                TapeOp::LayerNorm { a, eps, .. } => {
                    DagOp::LayerNorm(tape_to_dag[*a].expect("mapped"), *eps)
                }
            };
            let dag_id = nodes.len();
            nodes.push(DagNode {
                op,
                shape: tn.shape.clone(),
                dtype: tn.dtype,
                device: tn.device.clone(),
            });
            tape_to_dag[t_idx] = Some(dag_id);
        }

        let dag_out = tape_to_dag[out_idx].ok_or_else(|| {
            MoraError::Runtime("trace_dag: output node missing from DAG".to_string())
        })?;
        Ok(Self {
            input_specs,
            nodes,
            output: dag_out,
            fused: false,
            algebraic_rewrites: 0,
            compiled_plan: None,
        })
    }

    /// Compute a deterministic 64-bit FNV-1a structural fingerprint of this DAG (including constants).
    pub fn fingerprint(&self) -> u64 {
        let mut h = FnvHasher::new();
        h.write_u64(self.output as u64);
        h.write_u8(u8::from(self.fused));
        h.write_u64(self.input_specs.len() as u64);
        for (shape, dt, dev) in &self.input_specs {
            h.write_usize_slice(shape);
            h.write_str(dt.short_name());
            h.write_str(&dev.to_string());
        }
        h.write_u64(self.nodes.len() as u64);
        for node in &self.nodes {
            h.write_str(node.op.name());
            h.write_usize_slice(&node.shape);
            h.write_str(node.dtype.short_name());
            h.write_str(&node.device.to_string());
            for inp in node.op.inputs() {
                h.write_u64(inp as u64);
            }
            match &node.op {
                DagOp::Input(slot) => h.write_u64(*slot as u64),
                DagOp::ConstTensor(t) => {
                    for v in t.to_flat_vec() {
                        h.write_u64(v.to_bits());
                    }
                }
                DagOp::AddScalar(_, c)
                | DagOp::MulScalar(_, c)
                | DagOp::RmsNorm(_, c)
                | DagOp::LayerNorm(_, c) => h.write_u64(c.to_bits()),
                DagOp::SubScalar(_, c, r)
                | DagOp::DivScalar(_, c, r)
                | DagOp::PowScalar(_, c, r) => {
                    h.write_u64(c.to_bits());
                    h.write_u8(u8::from(*r));
                }
                DagOp::SumAxis(_, ax, kd) => {
                    h.write_u64(*ax as u64);
                    h.write_u8(u8::from(*kd));
                }
                DagOp::Reshape(_, sh) => h.write_usize_slice(sh),
                DagOp::Transpose(_, d0, d1) => {
                    h.write_u64(*d0 as u64);
                    h.write_u64(*d1 as u64);
                }
                DagOp::Gather(_, idxs) | DagOp::ScatterAdd(_, _, idxs) => h.write_usize_slice(idxs),
                DagOp::FusedLinearAct { act, .. } => h.write_str(act.name()),
                DagOp::FusedRmsNormLinear { eps, act, .. } => {
                    h.write_u64(eps.to_bits());
                    h.write_str(act.name());
                }
                DagOp::FusedFlashAttention {
                    scale,
                    k_is_transposed,
                    ..
                } => {
                    h.write_u64(scale.to_bits());
                    h.write_u8(u8::from(*k_is_transposed));
                }
                DagOp::FusedElementwise { steps, .. } => {
                    h.write_u64(steps.len() as u64);
                    for s in steps {
                        hash_ew_step(&mut h, s);
                    }
                }
                _ => {}
            }
        }
        h.finish()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn fused_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.op.is_fused()).count()
    }

    pub fn op_names(&self) -> Vec<&'static str> {
        self.nodes.iter().map(|n| n.op.name()).collect()
    }

    pub(crate) fn output_device(&self) -> Device {
        self.nodes
            .get(self.output)
            .map(|n| n.device.clone())
            .unwrap_or(Device::Cpu)
    }

    /// Compute the roofline FLOPs, bandwidth, fusion-savings, and native JIT cost report.
    pub(crate) fn cost_report(&self) -> DagCostReport {
        DagCostModel::analyze(self)
    }

    /// Compute the static buffer lifetime reuse plan for this DAG.
    pub fn memory_plan(&self) -> DagMemoryPlan {
        if let Some(plan) = &self.compiled_plan {
            return plan.memory_plan.clone();
        }
        DagMemoryPlan::build(self)
    }

    /// Compile (fuse + build `DagMemoryPlan` + allocate pooled workspace) using `cache`.
    pub fn compile_with_cache(&self, cache: &DagPlanCache) -> Self {
        let plan = cache.get_or_compile(self);
        let mut compiled = plan.dag.clone();
        compiled.compiled_plan = Some(plan);
        compiled
    }

    /// Generate a Graphviz DOT representation of this compute graph.
    pub fn to_dot(&self) -> String {
        let mem = self.memory_plan();
        let mut out = String::from("digraph TensorComputeDag {\n  rankdir=TB;\n");
        for (idx, n) in self.nodes.iter().enumerate() {
            let style = if n.op.is_fused() {
                ", style=filled, fillcolor=\"#dbeafe\""
            } else {
                ""
            };
            let slot_tag = match mem.node_slots.get(idx).copied().flatten() {
                Some(s) => format!(" [buf#{s}]"),
                None => String::new(),
            };
            let _ = writeln!(
                out,
                "  n{idx} [label=\"{}: {:?} {}{}\"{}];",
                n.op.name(),
                n.shape,
                n.dtype.short_name(),
                slot_tag,
                style
            );
            for inp in n.op.inputs() {
                let _ = writeln!(out, "  n{inp} -> n{idx};");
            }
        }
        let _ = writeln!(out, "}}");
        out
    }

    /// Apply Pass 0 DType-safe algebraic & layout simplification followed by all 4 operator-fusion
    /// passes (`FusedFlashAttention`, `FusedRmsNormLinear`, `FusedLinearAct`, `FusedElementwise`)
    /// and dead-node elimination.
    pub fn fuse(&self) -> Self {
        let mut nodes = self.nodes.clone();
        let mut out_id = self.output;
        let mut algebraic_rewrites = self.algebraic_rewrites;

        // ── Pass 0: DType-Safe Algebraic Simplification & Layout Cancellation ──
        // Strictly guards integer identities (`x + 0`, `x - 0`, `x * 1`, `x / 1`) by
        // `dtype.is_int()` so IEEE-754 floating-point semantics (`NaN`, `±Inf`, `-0.0 + 0.0`)
        // are NEVER broken.
        {
            let mut changed = true;
            let mut rounds = 0usize;
            while changed && rounds < 4 {
                changed = false;
                rounds += 1;
                for idx in 0..nodes.len() {
                    let dt = nodes[idx].dtype;
                    let new_op = match &nodes[idx].op {
                        DagOp::Transpose(a, d0, d1) if d0 == d1 => Some(DagOp::Identity(*a)),
                        DagOp::Transpose(a, d0, d1) => {
                            if let DagOp::Transpose(orig, inner0, inner1) = nodes[*a].op
                                && ((inner0 == *d0 && inner1 == *d1)
                                    || (inner0 == *d1 && inner1 == *d0))
                            {
                                Some(DagOp::Identity(orig))
                            } else {
                                None
                            }
                        }
                        DagOp::Reshape(a, target_sh) => {
                            if nodes[*a].shape == *target_sh {
                                Some(DagOp::Identity(*a))
                            } else if let DagOp::Reshape(orig, _) = nodes[*a].op {
                                if nodes[orig].shape == *target_sh {
                                    Some(DagOp::Identity(orig))
                                } else {
                                    Some(DagOp::Reshape(orig, target_sh.clone()))
                                }
                            } else {
                                None
                            }
                        }
                        // `x ** 2.0 -> x * x` is 100% IEEE-754 bit-exact across all finite, ±0.0, ±Inf, and NaN!
                        DagOp::PowScalar(a, c, false) if *c == 2.0 => Some(DagOp::Mul(*a, *a)),
                        // `x ** 1.0 -> x` is IEEE-754 bit-exact.
                        DagOp::PowScalar(a, c, false) if *c == 1.0 => Some(DagOp::Identity(*a)),
                        // `-(-x) -> x` is IEEE-754 bit-exact across all finite, ±0.0, ±Inf, and NaN!
                        DagOp::Neg(a) => {
                            if let DagOp::Neg(orig) = nodes[*a].op {
                                Some(DagOp::Identity(orig))
                            } else {
                                None
                            }
                        }
                        // Exact integer algebraic identities — NEVER applied to floating-point DTypes!
                        DagOp::AddScalar(a, c) if dt.is_int() && *c == 0.0 => {
                            Some(DagOp::Identity(*a))
                        }
                        DagOp::SubScalar(a, c, false) if dt.is_int() && *c == 0.0 => {
                            Some(DagOp::Identity(*a))
                        }
                        DagOp::MulScalar(a, c) if dt.is_int() && *c == 1.0 => {
                            Some(DagOp::Identity(*a))
                        }
                        DagOp::DivScalar(a, c, false) if dt.is_int() && *c == 1.0 => {
                            Some(DagOp::Identity(*a))
                        }
                        _ => None,
                    };
                    if let Some(op) = new_op {
                        nodes[idx].op = op;
                        algebraic_rewrites += 1;
                        changed = true;
                    }
                }
                // Forward all `DagOp::Identity` references directly to their root source!
                let mut forward: Vec<usize> = (0..nodes.len()).collect();
                for (idx, dst) in forward.iter_mut().enumerate() {
                    let mut cur = idx;
                    let mut hops = 0usize;
                    while let DagOp::Identity(src) = nodes[cur].op {
                        if hops >= nodes.len() {
                            break;
                        }
                        cur = src;
                        hops += 1;
                    }
                    *dst = cur;
                }
                if forward.iter().enumerate().any(|(i, &f)| i != f) {
                    for node in &mut nodes {
                        if !matches!(node.op, DagOp::Identity(_)) {
                            node.op.remap_inputs(&forward);
                        }
                    }
                    out_id = forward[out_id];
                }
            }
            let (simplified_nodes, simplified_out) = compact_live_nodes(nodes, out_id);
            nodes = simplified_nodes;
            out_id = simplified_out;
        }

        let count_uses = |ns: &[DagNode], out: usize| -> Vec<usize> {
            let mut uses = vec![0usize; ns.len()];
            uses[out] += 1;
            for n in ns {
                for u in n.op.inputs() {
                    uses[u] += 1;
                }
            }
            uses
        };

        // ── Pass 1: Fuse Scaled Dot-Product Attention into `FusedFlashAttention` ──
        // Matches: MatMul(Softmax(Add?(Scale?(MatMul(Q, Transpose?(K))), Mask?)), V)
        {
            let uses = count_uses(&nodes, out_id);
            for idx in 0..nodes.len() {
                let DagOp::MatMul(probs_id, v_id) = nodes[idx].op else {
                    continue;
                };
                if uses[probs_id] != 1 {
                    continue;
                }
                let DagOp::Softmax(logits_id) = nodes[probs_id].op else {
                    continue;
                };
                if uses[logits_id] != 1 {
                    continue;
                }

                // Optional additive mask: `scores + mask` or `mask + scores`
                let (scores_id, mask_opt) = match nodes[logits_id].op {
                    DagOp::Add(a, b) => {
                        if uses[a] == 1 && looks_like_qk_scores(&nodes, a) {
                            (a, Some(b))
                        } else if uses[b] == 1 && looks_like_qk_scores(&nodes, b) {
                            (b, Some(a))
                        } else {
                            (logits_id, None)
                        }
                    }
                    _ => (logits_id, None),
                };

                // Optional scaling: `* scale`, `/ denom`, `* ConstScalar`, `/ ConstScalar`
                let (raw_qk_id, scale) = match extract_attention_scale(&nodes, &uses, scores_id) {
                    Some((inner, s)) => (inner, s),
                    None => (scores_id, 1.0),
                };

                if raw_qk_id != logits_id && uses[raw_qk_id] != 1 {
                    continue;
                }
                let DagOp::MatMul(q_id, k_arg_id) = nodes[raw_qk_id].op else {
                    continue;
                };

                let q_node = &nodes[q_id];
                let k_arg_node = &nodes[k_arg_id];
                let v_node = &nodes[v_id];
                if q_node.shape.len() < 2
                    || k_arg_node.shape.len() < 2
                    || v_node.shape.len() < 2
                    || q_node.dtype != k_arg_node.dtype
                    || q_node.dtype != v_node.dtype
                    || q_node.device != k_arg_node.device
                    || q_node.device != v_node.device
                {
                    continue;
                }

                // Absorb trailing last-two-axes transpose on K when exclusive to Q@K^T!
                let (k_id, k_is_transposed) = if uses[k_arg_id] == 1
                    && let DagOp::Transpose(orig_k, d0, d1) = k_arg_node.op
                {
                    let kr = nodes[orig_k].shape.len();
                    if kr >= 2 && ((d0 == kr - 2 && d1 == kr - 1) || (d0 == kr - 1 && d1 == kr - 2))
                    {
                        (orig_k, false)
                    } else {
                        (k_arg_id, true)
                    }
                } else {
                    (k_arg_id, true)
                };

                let k_node = &nodes[k_id];
                let qr = q_node.shape.len();
                let kr = k_node.shape.len();
                let vr = v_node.shape.len();
                let d_k = q_node.shape[qr - 1];
                let (s_k, k_d) = if k_is_transposed {
                    (k_node.shape[kr - 1], k_node.shape[kr - 2])
                } else {
                    (k_node.shape[kr - 2], k_node.shape[kr - 1])
                };
                if d_k != k_d || s_k != v_node.shape[vr - 2] {
                    continue;
                }

                nodes[idx].op = DagOp::FusedFlashAttention {
                    q: q_id,
                    k: k_id,
                    v: v_id,
                    mask: mask_opt,
                    scale,
                    k_is_transposed,
                };
            }
        }

        let (mut nodes, compacted_out) = compact_live_nodes(nodes, out_id);
        out_id = compacted_out;

        // ── Pass 2: Fuse `FusedRmsNormLinear` & `FusedLinearAct` (2D and batched ND `[..., M, K] @ [K, N]`) ──
        {
            let uses = count_uses(&nodes, out_id);
            let as_act = |op: &DagOp| -> Option<(usize, FusedAct)> {
                match *op {
                    DagOp::Relu(a) => Some((a, FusedAct::Relu)),
                    DagOp::Gelu(a) => Some((a, FusedAct::Gelu)),
                    DagOp::Silu(a) => Some((a, FusedAct::Silu)),
                    DagOp::Tanh(a) => Some((a, FusedAct::Tanh)),
                    DagOp::Sigmoid(a) => Some((a, FusedAct::Sigmoid)),
                    _ => None,
                }
            };
            let is_linear_matmul = |ns: &[DagNode], mm_id: usize| -> Option<(usize, usize)> {
                if let DagOp::MatMul(x, w) = ns[mm_id].op
                    && ns[x].shape.len() >= 2
                    && ns[w].shape.len() == 2
                    && ns[mm_id].shape.len() == ns[x].shape.len()
                    && ns[x].shape.last() == Some(&ns[w].shape[0])
                    && ns[mm_id].shape.last() == Some(&ns[w].shape[1])
                    && ns[x].dtype == ns[w].dtype
                    && ns[x].device == ns[w].device
                {
                    Some((x, w))
                } else {
                    None
                }
            };

            let make_fused_linear_op = |ns: &[DagNode],
                                        use_cnts: &[usize],
                                        x: usize,
                                        w: usize,
                                        bias: Option<usize>,
                                        act: FusedAct|
             -> Option<DagOp> {
                if let Some((orig_x, eps, gamma)) = extract_rms_norm_source(ns, use_cnts, x) {
                    Some(DagOp::FusedRmsNormLinear {
                        x: orig_x,
                        eps,
                        gamma,
                        w,
                        bias,
                        act,
                    })
                } else if bias.is_some() || act != FusedAct::None {
                    Some(DagOp::FusedLinearAct { x, w, bias, act })
                } else {
                    None
                }
            };

            // Traverse top-down (reverse topological order) so `(X @ W + b).act()` matches the
            // outer activation + bias + matmul as a single `FusedLinearAct`/`FusedRmsNormLinear`
            // before the inner `Add` would be rewritten with `act = None`.
            for idx in (0..nodes.len()).rev() {
                if let Some((inner, act)) = as_act(&nodes[idx].op)
                    && uses[inner] == 1
                {
                    if let DagOp::Add(lhs, rhs) = nodes[inner].op {
                        if uses[lhs] == 1
                            && let Some((x, w)) = is_linear_matmul(&nodes, lhs)
                            && nodes[inner].shape == nodes[lhs].shape
                            && let Some(fused_op) =
                                make_fused_linear_op(&nodes, &uses, x, w, Some(rhs), act)
                        {
                            nodes[idx].op = fused_op;
                            continue;
                        }
                        if uses[rhs] == 1
                            && let Some((x, w)) = is_linear_matmul(&nodes, rhs)
                            && nodes[inner].shape == nodes[rhs].shape
                            && let Some(fused_op) =
                                make_fused_linear_op(&nodes, &uses, x, w, Some(lhs), act)
                        {
                            nodes[idx].op = fused_op;
                            continue;
                        }
                    }
                    if let Some((x, w)) = is_linear_matmul(&nodes, inner)
                        && let Some(fused_op) = make_fused_linear_op(&nodes, &uses, x, w, None, act)
                    {
                        nodes[idx].op = fused_op;
                        continue;
                    }
                }
                if let DagOp::Add(lhs, rhs) = nodes[idx].op {
                    if uses[lhs] == 1
                        && let Some((x, w)) = is_linear_matmul(&nodes, lhs)
                        && nodes[idx].shape == nodes[lhs].shape
                        && let Some(fused_op) =
                            make_fused_linear_op(&nodes, &uses, x, w, Some(rhs), FusedAct::None)
                    {
                        nodes[idx].op = fused_op;
                        continue;
                    }
                    if uses[rhs] == 1
                        && let Some((x, w)) = is_linear_matmul(&nodes, rhs)
                        && nodes[idx].shape == nodes[rhs].shape
                        && let Some(fused_op) =
                            make_fused_linear_op(&nodes, &uses, x, w, Some(lhs), FusedAct::None)
                    {
                        nodes[idx].op = fused_op;
                        continue;
                    }
                }
                // Bare `RMSNorm(x, eps) [* gamma] @ W` (no bias, no activation)
                if let Some((x, w)) = is_linear_matmul(&nodes, idx)
                    && let Some(fused_op) =
                        make_fused_linear_op(&nodes, &uses, x, w, None, FusedAct::None)
                {
                    nodes[idx].op = fused_op;
                }
            }
        }

        // Compact dead nodes before elementwise chain fusion.
        let (mut nodes, compacted_out) = compact_live_nodes(nodes, out_id);
        out_id = compacted_out;

        // ── Pass 3: Fuse maximal single-consumer chains of elementwise operations ──
        let uses = count_uses(&nodes, out_id);
        let is_elementwise = |op: &DagOp| -> bool {
            matches!(
                op,
                DagOp::Add(_, _)
                    | DagOp::Sub(_, _)
                    | DagOp::Mul(_, _)
                    | DagOp::Div(_, _)
                    | DagOp::Pow(_, _)
                    | DagOp::AddScalar(_, _)
                    | DagOp::SubScalar(_, _, _)
                    | DagOp::MulScalar(_, _)
                    | DagOp::DivScalar(_, _, _)
                    | DagOp::PowScalar(_, _, _)
                    | DagOp::Neg(_)
                    | DagOp::Relu(_)
                    | DagOp::Gelu(_)
                    | DagOp::Silu(_)
                    | DagOp::Tanh(_)
                    | DagOp::Sigmoid(_)
                    | DagOp::Exp(_)
                    | DagOp::Ln(_)
                    | DagOp::Sqrt(_)
                    | DagOp::Sin(_)
                    | DagOp::Cos(_)
                    | DagOp::Abs(_)
            )
        };

        let mut absorbed = vec![false; nodes.len()];
        for idx in (0..nodes.len()).rev() {
            if absorbed[idx] || !is_elementwise(&nodes[idx].op) {
                continue;
            }
            let mut cluster: Vec<usize> = Vec::new();
            let mut stack = vec![idx];
            let mut visited = vec![false; nodes.len()];
            while let Some(cur) = stack.pop() {
                if visited[cur] {
                    continue;
                }
                visited[cur] = true;
                cluster.push(cur);
                for pred in nodes[cur].op.inputs() {
                    if pred < idx
                        && !absorbed[pred]
                        && uses[pred] == 1
                        && is_elementwise(&nodes[pred].op)
                        && nodes[pred].dtype == nodes[idx].dtype
                        && nodes[pred].device == nodes[idx].device
                    {
                        stack.push(pred);
                    }
                }
            }
            if cluster.len() < 2 {
                continue;
            }
            cluster.sort_unstable();
            let cluster_set: HashMap<usize, usize> =
                cluster.iter().enumerate().map(|(i, &id)| (id, i)).collect();

            let mut ext_inputs: Vec<usize> = Vec::new();
            let mut ext_map: HashMap<usize, usize> = HashMap::new();
            for &nid in &cluster {
                for pred in nodes[nid].op.inputs() {
                    if !cluster_set.contains_key(&pred) && !ext_map.contains_key(&pred) {
                        let slot = ext_inputs.len();
                        ext_inputs.push(pred);
                        ext_map.insert(pred, slot);
                    }
                }
            }

            let n_ext = ext_inputs.len();
            let resolve_ref = |id: usize| -> usize {
                if let Some(&e_slot) = ext_map.get(&id) {
                    e_slot
                } else {
                    n_ext + cluster_set[&id]
                }
            };

            let mut steps = Vec::with_capacity(cluster.len());
            for &nid in &cluster {
                if nid != idx {
                    absorbed[nid] = true;
                }
                let step = match nodes[nid].op {
                    DagOp::Add(a, b) => FusedEwStep::Add(resolve_ref(a), resolve_ref(b)),
                    DagOp::Sub(a, b) => FusedEwStep::Sub(resolve_ref(a), resolve_ref(b)),
                    DagOp::Mul(a, b) => FusedEwStep::Mul(resolve_ref(a), resolve_ref(b)),
                    DagOp::Div(a, b) => FusedEwStep::Div(resolve_ref(a), resolve_ref(b)),
                    DagOp::Pow(a, b) => FusedEwStep::Pow(resolve_ref(a), resolve_ref(b)),
                    DagOp::AddScalar(a, c) => FusedEwStep::AddScalar(resolve_ref(a), c),
                    DagOp::SubScalar(a, c, r) => FusedEwStep::SubScalar(resolve_ref(a), c, r),
                    DagOp::MulScalar(a, c) => FusedEwStep::MulScalar(resolve_ref(a), c),
                    DagOp::DivScalar(a, c, r) => FusedEwStep::DivScalar(resolve_ref(a), c, r),
                    DagOp::PowScalar(a, c, r) => FusedEwStep::PowScalar(resolve_ref(a), c, r),
                    DagOp::Neg(a) => FusedEwStep::Neg(resolve_ref(a)),
                    DagOp::Relu(a) => FusedEwStep::Relu(resolve_ref(a)),
                    DagOp::Gelu(a) => FusedEwStep::Gelu(resolve_ref(a)),
                    DagOp::Silu(a) => FusedEwStep::Silu(resolve_ref(a)),
                    DagOp::Tanh(a) => FusedEwStep::Tanh(resolve_ref(a)),
                    DagOp::Sigmoid(a) => FusedEwStep::Sigmoid(resolve_ref(a)),
                    DagOp::Exp(a) => FusedEwStep::Exp(resolve_ref(a)),
                    DagOp::Ln(a) => FusedEwStep::Ln(resolve_ref(a)),
                    DagOp::Sqrt(a) => FusedEwStep::Sqrt(resolve_ref(a)),
                    DagOp::Sin(a) => FusedEwStep::Sin(resolve_ref(a)),
                    DagOp::Cos(a) => FusedEwStep::Cos(resolve_ref(a)),
                    DagOp::Abs(a) => FusedEwStep::Abs(resolve_ref(a)),
                    _ => unreachable!("only elementwise ops enter cluster"),
                };
                steps.push(step);
            }
            nodes[idx].op = DagOp::FusedElementwise {
                inputs: ext_inputs,
                steps,
            };
        }

        let (final_nodes, final_out) = compact_live_nodes(nodes, out_id);
        Self {
            input_specs: self.input_specs.clone(),
            nodes: final_nodes,
            output: final_out,
            fused: true,
            algebraic_rewrites,
            compiled_plan: None,
        }
    }

    /// Execute this compute graph on `inputs` while capturing nanosecond profile spans
    /// for the graph execution (and mirroring into the global [`crate::runtime::profiler`] if active).
    pub(crate) fn run_profiled(
        &self,
        cache: &DagPlanCache,
        inputs: &[Arc<Tensor>],
    ) -> MoraResult<(Arc<Tensor>, u64, Vec<crate::runtime::profiler::TraceEvent>)> {
        let start_ns = crate::runtime::profiler::now_ns();
        let t0 = std::time::Instant::now();
        let out = self.run_with_cache(cache, inputs)?;
        let dur_ns = t0.elapsed().as_nanos() as u64;
        let rep = self.cost_report();
        let args = vec![
            ("nodes".to_string(), rep.total_nodes.to_string()),
            ("fused_nodes".to_string(), rep.fused_nodes.to_string()),
            ("flops".to_string(), rep.total_flops.to_string()),
            ("bottleneck".to_string(), rep.bottleneck.to_string()),
            ("device".to_string(), self.output_device().to_string()),
        ];
        let mut events = Vec::with_capacity(self.nodes.len() + 1);
        let tid = crate::runtime::profiler::current_tid();
        events.push(crate::runtime::profiler::TraceEvent {
            cat: "compute".to_string(),
            name: format!("ComputeDag::run({} nodes)", self.nodes.len()),
            start_ns,
            dur_ns,
            tid,
            args: args.clone(),
        });
        let per_node = dur_ns / (self.nodes.len().max(1) as u64);
        for (i, n) in self.nodes.iter().enumerate() {
            events.push(crate::runtime::profiler::TraceEvent {
                cat: "compute".to_string(),
                name: format!("dag_node#{}::{}", i, n.op.name()),
                start_ns: start_ns + (i as u64) * per_node,
                dur_ns: per_node.max(1),
                tid,
                args: vec![
                    ("shape".to_string(), format!("{:?}", n.shape)),
                    ("dtype".to_string(), n.dtype.short_name().to_string()),
                ],
            });
        }
        if crate::runtime::profiler::is_active() {
            crate::runtime::profiler::record_span(
                "compute",
                &format!("ComputeDag::run({} nodes)", self.nodes.len()),
                start_ns,
                dur_ns,
                args,
                true,
            );
        }
        Ok((out, dur_ns, events))
    }

    /// Snapshot `(vram_scratch_allocs, vram_buffer_reuses)` observed across executions of this DAG's
    /// attached compiled plan (or `(0, 0)` if not yet compiled).
    pub(crate) fn vram_reuse_stats(&self) -> (u64, u64) {
        self.compiled_plan
            .as_ref()
            .map_or((0, 0), |p| p.vram_reuse_stats())
    }

    /// Execute this compute graph on `inputs` using attached [`CompiledDagPlan`] if present,
    /// otherwise building a [`DagMemoryPlan`] + scratch workspace directly.
    pub fn run(&self, inputs: &[Arc<Tensor>]) -> MoraResult<Arc<Tensor>> {
        if let Some(plan) = &self.compiled_plan {
            return plan.execute(inputs);
        }
        let mem = DagMemoryPlan::build(self);
        let const_flats: Vec<Option<Vec<f64>>> = self
            .nodes
            .iter()
            .map(|n| match &n.op {
                DagOp::ConstTensor(t) => Some(t.to_flat_vec()),
                _ => None,
            })
            .collect();
        let out_numel = self
            .nodes
            .get(self.output)
            .and_then(|n| checked_numel(&n.shape).ok())
            .unwrap_or(1);
        let mut ws = DagWorkspace::from_plan(&mem, out_numel);
        execute_dag_with_workspace(self, &mem, &const_flats, &mut ws, inputs, None)
    }

    /// Execute this compute graph using the runtime's [`DagPlanCache`] when compiled/fused,
    /// or directly when unfused (preserving exact unfused semantics for `dag.run(...)` before `.fuse()`).
    pub(crate) fn run_with_cache(
        &self,
        cache: &DagPlanCache,
        inputs: &[Arc<Tensor>],
    ) -> MoraResult<Arc<Tensor>> {
        if let Some(plan) = &self.compiled_plan {
            return plan.execute(inputs);
        }
        if self.fused {
            let plan = cache.get_or_compile(self);
            return plan.execute(inputs);
        }
        self.run(inputs)
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 5. Pattern-Matching & Workspace Execution Helpers
// ════════════════════════════════════════════════════════════════════════════

fn const_scalar_value(node: &DagNode) -> Option<f64> {
    if let DagOp::ConstTensor(t) = &node.op
        && t.numel() == 1
    {
        return Some(t.get_linear(0));
    }
    None
}

fn looks_like_qk_scores(nodes: &[DagNode], id: usize) -> bool {
    match nodes[id].op {
        DagOp::MatMul(_, _) => true,
        DagOp::MulScalar(a, _) | DagOp::DivScalar(a, _, false) => {
            matches!(nodes[a].op, DagOp::MatMul(_, _))
        }
        DagOp::Mul(a, b) => {
            (matches!(nodes[a].op, DagOp::MatMul(_, _)) && const_scalar_value(&nodes[b]).is_some())
                || (matches!(nodes[b].op, DagOp::MatMul(_, _))
                    && const_scalar_value(&nodes[a]).is_some())
        }
        DagOp::Div(a, b) => {
            matches!(nodes[a].op, DagOp::MatMul(_, _)) && const_scalar_value(&nodes[b]).is_some()
        }
        _ => false,
    }
}

fn extract_attention_scale(nodes: &[DagNode], uses: &[usize], id: usize) -> Option<(usize, f64)> {
    match nodes[id].op {
        DagOp::MulScalar(inner, s) => Some((inner, s)),
        DagOp::DivScalar(inner, d, false) if d != 0.0 => Some((inner, 1.0 / d)),
        DagOp::Mul(a, b) => {
            if uses[a] == 1
                && let Some(s) = const_scalar_value(&nodes[b])
            {
                Some((a, s))
            } else if uses[b] == 1
                && let Some(s) = const_scalar_value(&nodes[a])
            {
                Some((b, s))
            } else {
                None
            }
        }
        DagOp::Div(a, b) => {
            if uses[a] == 1
                && let Some(d) = const_scalar_value(&nodes[b])
                && d != 0.0
            {
                Some((a, 1.0 / d))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn extract_rms_norm_source(
    nodes: &[DagNode],
    uses: &[usize],
    x_id: usize,
) -> Option<(usize, f64, Option<usize>)> {
    if uses[x_id] != 1 {
        return None;
    }
    match nodes[x_id].op {
        DagOp::RmsNorm(orig_x, eps) if nodes[orig_x].shape == nodes[x_id].shape => {
            Some((orig_x, eps, None))
        }
        DagOp::Mul(lhs, rhs) => {
            if uses[lhs] == 1
                && let DagOp::RmsNorm(orig_x, eps) = nodes[lhs].op
                && nodes[orig_x].shape == nodes[x_id].shape
            {
                return Some((orig_x, eps, Some(rhs)));
            }
            if uses[rhs] == 1
                && let DagOp::RmsNorm(orig_x, eps) = nodes[rhs].op
                && nodes[orig_x].shape == nodes[x_id].shape
            {
                return Some((orig_x, eps, Some(lhs)));
            }
            None
        }
        _ => None,
    }
}

fn resolve_node_vram(
    dag: &TensorComputeDag,
    node_id: usize,
    inputs: &[Arc<Tensor>],
    mem: &DagMemoryPlan,
    slot_vram: &[Option<Arc<dyn DeviceVramBuffer>>],
) -> Option<Arc<dyn DeviceVramBuffer>> {
    match &dag.nodes[node_id].op {
        DagOp::Input(slot) => inputs[*slot].vram_buffer_if_contiguous(),
        DagOp::ConstTensor(t) => t.vram_buffer_if_contiguous(),
        _ => mem
            .node_slots
            .get(node_id)
            .copied()
            .flatten()
            .and_then(|s_id| slot_vram[s_id].clone()),
    }
}

fn try_eval_node_vram(
    backend: &dyn DeviceBackend,
    dag: &TensorComputeDag,
    node: &DagNode,
    inputs: &[Arc<Tensor>],
    mem: &DagMemoryPlan,
    slot_vram: &[Option<Arc<dyn DeviceVramBuffer>>],
    dst_vram: Option<Arc<dyn DeviceVramBuffer>>,
) -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
    if !backend.supports_vram_residency() {
        return Ok(None);
    }
    let get_v = |id: usize| resolve_node_vram(dag, id, inputs, mem, slot_vram);
    let shape_of = |id: usize| dag.nodes[id].shape.as_slice();
    let out_numel = checked_numel(&node.shape)?;

    // Ensure `dst_vram` never aliases any live input handle before passing it as an in-place target.
    let safe_dst = dst_vram.filter(|dst| {
        let dst_h = dst.raw_handle();
        !node
            .op
            .inputs()
            .into_iter()
            .any(|pred| get_v(pred).is_some_and(|b| b.raw_handle() == dst_h))
    });

    let run_ew_vram = |ids: &[usize],
                       steps: &[FusedEwStep],
                       dst: Option<Arc<dyn DeviceVramBuffer>>|
     -> MoraResult<Option<Arc<dyn DeviceVramBuffer>>> {
        let mut owned = Vec::with_capacity(ids.len());
        for &id in ids {
            let Some(buf) = get_v(id) else {
                return Ok(None);
            };
            if buf.device() != &node.device {
                return Ok(None);
            }
            owned.push(buf);
        }
        let refs: Vec<&dyn DeviceVramBuffer> = owned.iter().map(|b| b.as_ref()).collect();
        let shapes: Vec<&[usize]> = ids.iter().map(|&id| shape_of(id)).collect();
        backend.run_fused_elementwise_vram(
            &refs,
            &shapes,
            steps,
            &node.shape,
            out_numel,
            node.dtype,
            dst,
        )
    };

    match &node.op {
        DagOp::Input(_) | DagOp::ConstTensor(_) => Ok(None),
        DagOp::Reshape(a, _) | DagOp::Identity(a) => Ok(get_v(*a)),
        DagOp::MatMul(a, b) => {
            let (Some(va), Some(vb)) = (get_v(*a), get_v(*b)) else {
                return Ok(None);
            };
            backend.run_matmul_vram(
                va.as_ref(),
                vb.as_ref(),
                shape_of(*a),
                shape_of(*b),
                node.dtype,
                safe_dst,
            )
        }
        DagOp::FusedLinearAct { x, w, bias, act } => {
            let (Some(vx), Some(vw)) = (get_v(*x), get_v(*w)) else {
                return Ok(None);
            };
            let vbias = match bias {
                None => None,
                Some(b_id) => {
                    let Some(vb) = get_v(*b_id) else {
                        return Ok(None);
                    };
                    Some((vb, shape_of(*b_id)))
                }
            };
            let x_shape = shape_of(*x);
            let w_shape = shape_of(*w);
            let k = w_shape[0];
            let n = w_shape[1];
            let m_total = checked_numel(x_shape)? / k.max(1);
            let bias_ref = vbias.as_ref().map(|(b, s)| (b.as_ref(), *s));
            backend.run_fused_linear_act_vram(
                vx.as_ref(),
                vw.as_ref(),
                bias_ref,
                *act,
                m_total,
                k,
                n,
                &node.shape,
                node.dtype,
                safe_dst,
            )
        }
        DagOp::FusedRmsNormLinear {
            x,
            eps,
            gamma,
            w,
            bias,
            act,
        } => {
            let (Some(vx), Some(vw)) = (get_v(*x), get_v(*w)) else {
                return Ok(None);
            };
            let vgamma = match gamma {
                None => None,
                Some(g_id) => {
                    let Some(vg) = get_v(*g_id) else {
                        return Ok(None);
                    };
                    Some((vg, shape_of(*g_id)))
                }
            };
            let vbias = match bias {
                None => None,
                Some(b_id) => {
                    let Some(vb) = get_v(*b_id) else {
                        return Ok(None);
                    };
                    Some((vb, shape_of(*b_id)))
                }
            };
            let x_shape = shape_of(*x);
            let w_shape = shape_of(*w);
            let k = w_shape[0];
            let n = w_shape[1];
            let m_total = checked_numel(x_shape)? / k.max(1);
            let gamma_ref = vgamma.as_ref().map(|(g, s)| (g.as_ref(), *s));
            let bias_ref = vbias.as_ref().map(|(b, s)| (b.as_ref(), *s));
            backend.run_fused_rms_norm_linear_vram(
                vx.as_ref(),
                *eps,
                gamma_ref,
                vw.as_ref(),
                bias_ref,
                *act,
                m_total,
                k,
                n,
                x_shape,
                &node.shape,
                node.dtype,
                safe_dst,
            )
        }
        DagOp::FusedFlashAttention {
            q,
            k,
            v,
            mask,
            scale,
            k_is_transposed,
        } => {
            let (Some(vq), Some(vk), Some(vv)) = (get_v(*q), get_v(*k), get_v(*v)) else {
                return Ok(None);
            };
            let vmask = match mask {
                None => None,
                Some(m_id) => {
                    let Some(vm) = get_v(*m_id) else {
                        return Ok(None);
                    };
                    Some((vm, shape_of(*m_id)))
                }
            };
            let mask_ref = vmask.as_ref().map(|(m, s)| (m.as_ref(), *s));
            backend.run_fused_flash_attention_vram(
                vq.as_ref(),
                vk.as_ref(),
                vv.as_ref(),
                mask_ref,
                shape_of(*q),
                shape_of(*k),
                shape_of(*v),
                &node.shape,
                *scale,
                *k_is_transposed,
                node.dtype,
                safe_dst,
            )
        }
        DagOp::FusedElementwise { inputs: ids, steps } => run_ew_vram(ids, steps, safe_dst),
        DagOp::Add(a, b) => run_ew_vram(&[*a, *b], &[FusedEwStep::Add(0, 1)], safe_dst),
        DagOp::Sub(a, b) => run_ew_vram(&[*a, *b], &[FusedEwStep::Sub(0, 1)], safe_dst),
        DagOp::Mul(a, b) => run_ew_vram(&[*a, *b], &[FusedEwStep::Mul(0, 1)], safe_dst),
        DagOp::Div(a, b) => run_ew_vram(&[*a, *b], &[FusedEwStep::Div(0, 1)], safe_dst),
        DagOp::Pow(a, b) => run_ew_vram(&[*a, *b], &[FusedEwStep::Pow(0, 1)], safe_dst),
        DagOp::AddScalar(a, c) => run_ew_vram(&[*a], &[FusedEwStep::AddScalar(0, *c)], safe_dst),
        DagOp::SubScalar(a, c, r) => {
            run_ew_vram(&[*a], &[FusedEwStep::SubScalar(0, *c, *r)], safe_dst)
        }
        DagOp::MulScalar(a, c) => run_ew_vram(&[*a], &[FusedEwStep::MulScalar(0, *c)], safe_dst),
        DagOp::DivScalar(a, c, r) => {
            run_ew_vram(&[*a], &[FusedEwStep::DivScalar(0, *c, *r)], safe_dst)
        }
        DagOp::PowScalar(a, c, r) => {
            run_ew_vram(&[*a], &[FusedEwStep::PowScalar(0, *c, *r)], safe_dst)
        }
        DagOp::Neg(a) => run_ew_vram(&[*a], &[FusedEwStep::Neg(0)], safe_dst),
        DagOp::Relu(a) => run_ew_vram(&[*a], &[FusedEwStep::Relu(0)], safe_dst),
        DagOp::Gelu(a) => run_ew_vram(&[*a], &[FusedEwStep::Gelu(0)], safe_dst),
        DagOp::Silu(a) => run_ew_vram(&[*a], &[FusedEwStep::Silu(0)], safe_dst),
        DagOp::Tanh(a) => run_ew_vram(&[*a], &[FusedEwStep::Tanh(0)], safe_dst),
        DagOp::Sigmoid(a) => run_ew_vram(&[*a], &[FusedEwStep::Sigmoid(0)], safe_dst),
        DagOp::Exp(a) => run_ew_vram(&[*a], &[FusedEwStep::Exp(0)], safe_dst),
        DagOp::Ln(a) => run_ew_vram(&[*a], &[FusedEwStep::Ln(0)], safe_dst),
        DagOp::Sqrt(a) => run_ew_vram(&[*a], &[FusedEwStep::Sqrt(0)], safe_dst),
        DagOp::Sin(a) => run_ew_vram(&[*a], &[FusedEwStep::Sin(0)], safe_dst),
        DagOp::Cos(a) => run_ew_vram(&[*a], &[FusedEwStep::Cos(0)], safe_dst),
        DagOp::Abs(a) => run_ew_vram(&[*a], &[FusedEwStep::Abs(0)], safe_dst),
        _ => Ok(None),
    }
}

fn execute_dag_with_workspace(
    dag: &TensorComputeDag,
    mem: &DagMemoryPlan,
    const_flats: &[Option<Vec<f64>>],
    ws: &mut DagWorkspace,
    inputs: &[Arc<Tensor>],
    vram_stats: Option<(&AtomicU64, &AtomicU64)>,
) -> MoraResult<Arc<Tensor>> {
    if inputs.len() != dag.input_specs.len() {
        return Err(MoraError::Runtime(format!(
            "ComputeDag.run: expected {} input tensor(s), got {}",
            dag.input_specs.len(),
            inputs.len()
        )));
    }
    for (i, (inp, (exp_shape, exp_dt, exp_dev))) in
        inputs.iter().zip(dag.input_specs.iter()).enumerate()
    {
        if &inp.shape != exp_shape || inp.dtype != *exp_dt || &inp.device != exp_dev {
            return Err(MoraError::Runtime(format!(
                "ComputeDag.run: input #{i} expected shape {:?} dtype {} device {}, got shape {:?} dtype {} device {}",
                exp_shape,
                exp_dt.short_name(),
                exp_dev,
                inp.shape,
                inp.dtype.short_name(),
                inp.device
            )));
        }
    }

    // Lazily decoded host views of inputs (never materialized on pure VRAM-to-VRAM GPU execution!).
    let mut input_flats: Vec<Option<Vec<f64>>> = vec![None; inputs.len()];
    let mut slot_vram: Vec<Option<Arc<dyn DeviceVramBuffer>>> = vec![None; mem.slots.len()];
    let mut slot_host_valid: Vec<bool> = vec![false; mem.slots.len()];
    let mut slot_meta: Vec<(usize, DType)> = vec![(0, DType::F64); mem.slots.len()];

    for (idx, node) in dag.nodes.iter().enumerate() {
        if matches!(node.op, DagOp::Input(_) | DagOp::ConstTensor(_)) {
            if idx == dag.output {
                return match &node.op {
                    DagOp::Input(slot) => Ok(Arc::clone(&inputs[*slot])),
                    DagOp::ConstTensor(t) => Ok(Arc::clone(t)),
                    _ => unreachable!(),
                };
            }
            continue;
        }

        let dst_slot_opt = mem.node_slots[idx];

        // ── Zero-copy GPU VRAM-to-VRAM fast path with DagMemoryPlan physical VRAM slot pooling ──
        let backend = backend_for_device(&node.device);
        if backend.supports_vram_residency() {
            let is_view_alias = matches!(node.op, DagOp::Reshape(_, _) | DagOp::Identity(_));
            let (pooled_dst, was_reused_slot) = if !is_view_alias
                && idx != dag.output
                && let Some(s_id) = dst_slot_opt
            {
                let slot_plan = &mem.slots[s_id];
                let slot_bytes = slot_plan.bytes.max(16);
                if let Some(existing) = &ws.vram_slots[s_id]
                    && existing.device() == &node.device
                    && existing.byte_len() >= slot_bytes
                {
                    (Some(Arc::clone(existing)), true)
                } else if let Some(fresh) = backend.alloc_vram_buffer(slot_bytes)? {
                    ws.vram_slots[s_id] = Some(Arc::clone(&fresh));
                    if let Some((allocs, _)) = vram_stats {
                        allocs.fetch_add(1, Ordering::Relaxed);
                    }
                    (Some(fresh), false)
                } else {
                    (None, false)
                }
            } else {
                (None, false)
            };

            let pooled_handle = pooled_dst.as_ref().map(|b| b.raw_handle());
            if let Some(out_vram) = try_eval_node_vram(
                backend.as_ref(),
                dag,
                node,
                inputs,
                mem,
                &slot_vram,
                pooled_dst,
            )? {
                if was_reused_slot
                    && pooled_handle.is_some_and(|h| h == out_vram.raw_handle())
                    && let Some((_, reuses)) = vram_stats
                {
                    reuses.fetch_add(1, Ordering::Relaxed);
                }
                if idx == dag.output {
                    return Tensor::from_vram_output(
                        out_vram,
                        node.shape.clone(),
                        node.dtype,
                        node.device.clone(),
                        false,
                    );
                }
                if let Some(s_id) = dst_slot_opt {
                    let numel = checked_numel(&node.shape)?;
                    slot_vram[s_id] = Some(out_vram);
                    slot_host_valid[s_id] = false;
                    slot_meta[s_id] = (numel, node.dtype);
                }
                continue;
            }
        }

        // Materialize host slices lazily for any predecessors required by host/hybrid dispatch.
        for pred in node.op.inputs() {
            match &dag.nodes[pred].op {
                DagOp::Input(slot) => {
                    if input_flats[*slot].is_none() {
                        input_flats[*slot] = Some(inputs[*slot].to_flat_vec());
                    }
                }
                DagOp::ConstTensor(_) => {}
                _ => {
                    if let Some(s_id) = mem.node_slots[pred]
                        && !slot_host_valid[s_id]
                        && let Some(vbuf) = &slot_vram[s_id]
                    {
                        let (numel, dt) = slot_meta[s_id];
                        let needed_bytes = dt.storage_bytes(numel);
                        let mut raw = vec![0u8; vbuf.byte_len()];
                        vbuf.download_bytes(&mut raw)?;
                        let aligned =
                            AlignedByteBuffer::from_raw_bytes(&raw[..needed_bytes], numel, dt)?;
                        let dst_slot = &mut ws.slots[s_id];
                        dst_slot.clear();
                        dst_slot.reserve(numel);
                        for i in 0..numel {
                            dst_slot.push(aligned.get_f64(i));
                        }
                        slot_host_valid[s_id] = true;
                    }
                }
            }
        }

        // Move the physical destination buffer out via O(1) swap so we can borrow all live source slots immutably!
        let mut dst_buf: Vec<f64> = if let Some(s_id) = dst_slot_opt {
            debug_assert!(
                !node
                    .op
                    .inputs()
                    .into_iter()
                    .any(|pred| mem.node_slots[pred] == Some(s_id)),
                "DagMemoryPlan aliasing violation: input shares destination buffer slot {s_id}"
            );
            std::mem::take(&mut ws.slots[s_id])
        } else {
            std::mem::take(&mut ws.output_buf)
        };

        let eval_res = eval_node_into(
            dag,
            node,
            &input_flats,
            const_flats,
            mem,
            &ws.slots,
            &mut dst_buf,
        );

        let finalize_slot = |ws_ref: &mut DagWorkspace, buf: Vec<f64>| {
            if let Some(s_id) = dst_slot_opt {
                ws_ref.slots[s_id] = buf;
            } else {
                ws_ref.output_buf = buf;
            }
        };

        if let Err(e) = eval_res {
            finalize_slot(ws, dst_buf);
            return Err(e);
        }

        if idx == dag.output {
            let out_tensor = Tensor::from_raw(
                dst_buf.clone(),
                node.shape.clone(),
                node.dtype,
                node.device.clone(),
                false,
            );
            finalize_slot(ws, dst_buf);
            return out_tensor;
        }
        finalize_slot(ws, dst_buf);
        if let Some(s_id) = dst_slot_opt {
            slot_host_valid[s_id] = true;
            slot_vram[s_id] = None;
        }
    }

    Err(MoraError::Runtime(
        "ComputeDag.run: missing output tensor".to_string(),
    ))
}

#[inline]
fn read_node_slice<'a>(
    dag: &'a TensorComputeDag,
    node_id: usize,
    input_flats: &'a [Option<Vec<f64>>],
    const_flats: &'a [Option<Vec<f64>>],
    mem: &DagMemoryPlan,
    slots: &'a [Vec<f64>],
) -> &'a [f64] {
    match &dag.nodes[node_id].op {
        DagOp::Input(slot) => input_flats[*slot]
            .as_deref()
            .expect("input flat lazily populated"),
        DagOp::ConstTensor(_) => const_flats[node_id]
            .as_deref()
            .expect("const_flat pre-decoded"),
        _ => {
            let s_id = mem.node_slots[node_id].expect("intermediate buffer slot assigned");
            &slots[s_id]
        }
    }
}

fn eval_node_into(
    dag: &TensorComputeDag,
    node: &DagNode,
    input_flats: &[Option<Vec<f64>>],
    const_flats: &[Option<Vec<f64>>],
    mem: &DagMemoryPlan,
    slots: &[Vec<f64>],
    dst: &mut Vec<f64>,
) -> MoraResult<()> {
    let backend = backend_for_device(&node.device);
    let out_numel = checked_numel(&node.shape)?;
    let get = |id: usize| read_node_slice(dag, id, input_flats, const_flats, mem, slots);
    let shape_of = |id: usize| dag.nodes[id].shape.as_slice();

    match &node.op {
        DagOp::Input(_) | DagOp::ConstTensor(_) => Ok(()),
        DagOp::FusedLinearAct { x, w, bias, act } => {
            let x_shape = shape_of(*x);
            let w_shape = shape_of(*w);
            let k = w_shape[0];
            let n = w_shape[1];
            let m_total = checked_numel(x_shape)? / k.max(1);
            let bias_arg = bias.map(|b| (get(b), shape_of(b)));
            backend.run_fused_linear_act_into(
                get(*x),
                get(*w),
                bias_arg,
                *act,
                m_total,
                k,
                n,
                &node.shape,
                node.dtype,
                dst,
            )
        }
        DagOp::FusedRmsNormLinear {
            x,
            eps,
            gamma,
            w,
            bias,
            act,
        } => {
            let x_shape = shape_of(*x);
            let w_shape = shape_of(*w);
            let k = w_shape[0];
            let n = w_shape[1];
            let m_total = checked_numel(x_shape)? / k.max(1);
            let gamma_arg = gamma.map(|g| (get(g), shape_of(g)));
            let bias_arg = bias.map(|b| (get(b), shape_of(b)));
            backend.run_fused_rms_norm_linear_into(
                get(*x),
                *eps,
                gamma_arg,
                get(*w),
                bias_arg,
                *act,
                m_total,
                k,
                n,
                x_shape,
                &node.shape,
                node.dtype,
                dst,
            )
        }
        DagOp::FusedFlashAttention {
            q,
            k,
            v,
            mask,
            scale,
            k_is_transposed,
        } => {
            let mask_arg = mask.map(|m| (get(m), shape_of(m)));
            backend.run_fused_flash_attention_into(
                get(*q),
                get(*k),
                get(*v),
                mask_arg,
                shape_of(*q),
                shape_of(*k),
                shape_of(*v),
                &node.shape,
                *scale,
                *k_is_transposed,
                node.dtype,
                dst,
            )
        }
        DagOp::FusedElementwise { inputs: ids, steps } => {
            let flats: Vec<&[f64]> = ids.iter().map(|&id| get(id)).collect();
            let shapes: Vec<&[usize]> = ids.iter().map(|&id| shape_of(id)).collect();
            backend.run_fused_elementwise_into(
                &flats,
                &shapes,
                steps,
                &node.shape,
                out_numel,
                node.dtype,
                dst,
            )
        }
        DagOp::Add(a, b) => backend.run_fused_elementwise_into(
            &[get(*a), get(*b)],
            &[shape_of(*a), shape_of(*b)],
            &[FusedEwStep::Add(0, 1)],
            &node.shape,
            out_numel,
            node.dtype,
            dst,
        ),
        DagOp::Sub(a, b) => backend.run_fused_elementwise_into(
            &[get(*a), get(*b)],
            &[shape_of(*a), shape_of(*b)],
            &[FusedEwStep::Sub(0, 1)],
            &node.shape,
            out_numel,
            node.dtype,
            dst,
        ),
        DagOp::Mul(a, b) => backend.run_fused_elementwise_into(
            &[get(*a), get(*b)],
            &[shape_of(*a), shape_of(*b)],
            &[FusedEwStep::Mul(0, 1)],
            &node.shape,
            out_numel,
            node.dtype,
            dst,
        ),
        DagOp::Div(a, b) => backend.run_fused_elementwise_into(
            &[get(*a), get(*b)],
            &[shape_of(*a), shape_of(*b)],
            &[FusedEwStep::Div(0, 1)],
            &node.shape,
            out_numel,
            node.dtype,
            dst,
        ),
        DagOp::Pow(a, b) => backend.run_fused_elementwise_into(
            &[get(*a), get(*b)],
            &[shape_of(*a), shape_of(*b)],
            &[FusedEwStep::Pow(0, 1)],
            &node.shape,
            out_numel,
            node.dtype,
            dst,
        ),
        DagOp::AddScalar(a, c) => backend.run_fused_elementwise_into(
            &[get(*a)],
            &[shape_of(*a)],
            &[FusedEwStep::AddScalar(0, *c)],
            &node.shape,
            out_numel,
            node.dtype,
            dst,
        ),
        DagOp::SubScalar(a, c, r) => backend.run_fused_elementwise_into(
            &[get(*a)],
            &[shape_of(*a)],
            &[FusedEwStep::SubScalar(0, *c, *r)],
            &node.shape,
            out_numel,
            node.dtype,
            dst,
        ),
        DagOp::MulScalar(a, c) => backend.run_fused_elementwise_into(
            &[get(*a)],
            &[shape_of(*a)],
            &[FusedEwStep::MulScalar(0, *c)],
            &node.shape,
            out_numel,
            node.dtype,
            dst,
        ),
        DagOp::DivScalar(a, c, r) => backend.run_fused_elementwise_into(
            &[get(*a)],
            &[shape_of(*a)],
            &[FusedEwStep::DivScalar(0, *c, *r)],
            &node.shape,
            out_numel,
            node.dtype,
            dst,
        ),
        DagOp::PowScalar(a, c, r) => backend.run_fused_elementwise_into(
            &[get(*a)],
            &[shape_of(*a)],
            &[FusedEwStep::PowScalar(0, *c, *r)],
            &node.shape,
            out_numel,
            node.dtype,
            dst,
        ),
        DagOp::Neg(a) => run_single_ew_into(
            backend.as_ref(),
            get(*a),
            shape_of(*a),
            FusedEwStep::Neg(0),
            node,
            out_numel,
            dst,
        ),
        DagOp::Relu(a) => run_single_ew_into(
            backend.as_ref(),
            get(*a),
            shape_of(*a),
            FusedEwStep::Relu(0),
            node,
            out_numel,
            dst,
        ),
        DagOp::Gelu(a) => run_single_ew_into(
            backend.as_ref(),
            get(*a),
            shape_of(*a),
            FusedEwStep::Gelu(0),
            node,
            out_numel,
            dst,
        ),
        DagOp::Silu(a) => run_single_ew_into(
            backend.as_ref(),
            get(*a),
            shape_of(*a),
            FusedEwStep::Silu(0),
            node,
            out_numel,
            dst,
        ),
        DagOp::Tanh(a) => run_single_ew_into(
            backend.as_ref(),
            get(*a),
            shape_of(*a),
            FusedEwStep::Tanh(0),
            node,
            out_numel,
            dst,
        ),
        DagOp::Sigmoid(a) => run_single_ew_into(
            backend.as_ref(),
            get(*a),
            shape_of(*a),
            FusedEwStep::Sigmoid(0),
            node,
            out_numel,
            dst,
        ),
        DagOp::Exp(a) => run_single_ew_into(
            backend.as_ref(),
            get(*a),
            shape_of(*a),
            FusedEwStep::Exp(0),
            node,
            out_numel,
            dst,
        ),
        DagOp::Ln(a) => run_single_ew_into(
            backend.as_ref(),
            get(*a),
            shape_of(*a),
            FusedEwStep::Ln(0),
            node,
            out_numel,
            dst,
        ),
        DagOp::Sqrt(a) => run_single_ew_into(
            backend.as_ref(),
            get(*a),
            shape_of(*a),
            FusedEwStep::Sqrt(0),
            node,
            out_numel,
            dst,
        ),
        DagOp::Sin(a) => run_single_ew_into(
            backend.as_ref(),
            get(*a),
            shape_of(*a),
            FusedEwStep::Sin(0),
            node,
            out_numel,
            dst,
        ),
        DagOp::Cos(a) => run_single_ew_into(
            backend.as_ref(),
            get(*a),
            shape_of(*a),
            FusedEwStep::Cos(0),
            node,
            out_numel,
            dst,
        ),
        DagOp::Abs(a) => run_single_ew_into(
            backend.as_ref(),
            get(*a),
            shape_of(*a),
            FusedEwStep::Abs(0),
            node,
            out_numel,
            dst,
        ),
        DagOp::StepPos(a) => {
            let src = get(*a);
            dst.clear();
            dst.reserve(src.len());
            for &v in src {
                let out_v = if v > 0.0 { 1.0 } else { 0.0 };
                dst.push(quantize_for_dtype(out_v, node.dtype));
            }
            Ok(())
        }
        DagOp::Sign(a) => {
            let src = get(*a);
            dst.clear();
            dst.reserve(src.len());
            for &v in src {
                let out_v = if v > 0.0 {
                    1.0
                } else if v < 0.0 {
                    -1.0
                } else {
                    0.0
                };
                dst.push(quantize_for_dtype(out_v, node.dtype));
            }
            Ok(())
        }
        DagOp::MatMul(a, b) => backend.run_matmul_into(
            get(*a),
            get(*b),
            shape_of(*a),
            shape_of(*b),
            node.dtype,
            dst,
        ),
        DagOp::Sum(a) => {
            let s: f64 = get(*a).iter().sum();
            dst.clear();
            dst.push(quantize_for_dtype(s, node.dtype));
            Ok(())
        }
        DagOp::Mean(a) => {
            let src = get(*a);
            let m = if src.is_empty() {
                0.0
            } else {
                src.iter().sum::<f64>() / (src.len() as f64)
            };
            dst.clear();
            dst.push(quantize_for_dtype(m, node.dtype));
            Ok(())
        }
        DagOp::SumAxis(a, axis, keepdim) => {
            let vals = sum_axis_flat(get(*a), shape_of(*a), *axis, *keepdim);
            dst.clear();
            dst.extend(vals.into_iter().map(|v| quantize_for_dtype(v, node.dtype)));
            Ok(())
        }
        DagOp::Reshape(a, _) | DagOp::Identity(a) => {
            dst.clear();
            dst.extend_from_slice(get(*a));
            Ok(())
        }
        DagOp::Transpose(a, d0, d1) => {
            let tr = transpose_flat(get(*a), shape_of(*a), *d0, *d1, &node.shape);
            dst.clear();
            dst.extend_from_slice(&tr);
            Ok(())
        }
        DagOp::Gather(a, indices) => {
            let src = get(*a);
            dst.clear();
            dst.reserve(indices.len());
            for &i in indices {
                dst.push(src[i]);
            }
            Ok(())
        }
        DagOp::ScatterAdd(a, out_n, indices) => {
            let sc = scatter_add_flat(get(*a), *out_n, indices);
            dst.clear();
            dst.extend_from_slice(&sc);
            Ok(())
        }
        DagOp::Softmax(a) => {
            let src = get(*a);
            let d = node.shape.last().copied().unwrap_or(1).max(1);
            dst.resize(src.len(), 0.0);
            for row in 0..(src.len() / d) {
                let base = row * d;
                let row_in = &src[base..base + d];
                let max_v = row_in.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let mut sum_e = 0.0;
                for j in 0..d {
                    let e = (row_in[j] - max_v).exp();
                    dst[base + j] = e;
                    sum_e += e;
                }
                let inv = if sum_e > 0.0 { 1.0 / sum_e } else { 0.0 };
                for j in 0..d {
                    dst[base + j] = quantize_for_dtype(dst[base + j] * inv, node.dtype);
                }
            }
            Ok(())
        }
        DagOp::RmsNorm(a, eps) => {
            let src = get(*a);
            let d = node.shape.last().copied().unwrap_or(1).max(1);
            let d_f = d as f64;
            dst.resize(src.len(), 0.0);
            for row in 0..(src.len() / d) {
                let base = row * d;
                let row_in = &src[base..base + d];
                let ms = simd_kernels::sum_sq_f64(row_in) / d_f;
                let inv_rms = 1.0 / (ms + *eps).sqrt();
                for j in 0..d {
                    dst[base + j] = quantize_for_dtype(row_in[j] * inv_rms, node.dtype);
                }
            }
            Ok(())
        }
        DagOp::LayerNorm(a, eps) => {
            let src = get(*a);
            let d = node.shape.last().copied().unwrap_or(1).max(1);
            let d_f = d as f64;
            dst.resize(src.len(), 0.0);
            for row in 0..(src.len() / d) {
                let base = row * d;
                let row_in = &src[base..base + d];
                let mean = row_in.iter().sum::<f64>() / d_f;
                let var = row_in
                    .iter()
                    .map(|x| {
                        let c = x - mean;
                        c * c
                    })
                    .sum::<f64>()
                    / d_f;
                let inv_std = 1.0 / (var + *eps).sqrt();
                for j in 0..d {
                    dst[base + j] = quantize_for_dtype((row_in[j] - mean) * inv_std, node.dtype);
                }
            }
            Ok(())
        }
    }
}

fn run_single_ew_into(
    backend: &dyn DeviceBackend,
    a: &[f64],
    a_shape: &[usize],
    step: FusedEwStep,
    node: &DagNode,
    out_numel: usize,
    dst: &mut Vec<f64>,
) -> MoraResult<()> {
    backend.run_fused_elementwise_into(
        &[a],
        &[a_shape],
        &[step],
        &node.shape,
        out_numel,
        node.dtype,
        dst,
    )
}

fn compact_live_nodes(nodes: Vec<DagNode>, output: usize) -> (Vec<DagNode>, usize) {
    let mut live = vec![false; nodes.len()];
    let mut stack = vec![output];
    while let Some(id) = stack.pop() {
        if live[id] {
            continue;
        }
        live[id] = true;
        for pred in nodes[id].op.inputs() {
            stack.push(pred);
        }
    }
    let mut remap = vec![0usize; nodes.len()];
    let mut compacted = Vec::new();
    for (old_id, mut node) in nodes.into_iter().enumerate() {
        if !live[old_id] {
            continue;
        }
        node.op.remap_inputs(&remap);
        remap[old_id] = compacted.len();
        compacted.push(node);
    }
    (compacted, remap[output])
}

/// Deterministic 64-bit FNV-1a hasher for structural DAG caching (stable across runs & platforms).
struct FnvHasher(u64);

impl FnvHasher {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    fn new() -> Self {
        Self(Self::OFFSET)
    }

    #[inline]
    fn write_u8(&mut self, b: u8) {
        self.0 ^= u64::from(b);
        self.0 = self.0.wrapping_mul(Self::PRIME);
    }

    #[inline]
    fn write_u64(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.write_u8(b);
        }
    }

    fn write_str(&mut self, s: &str) {
        self.write_u64(s.len() as u64);
        for &b in s.as_bytes() {
            self.write_u8(b);
        }
    }

    fn write_usize_slice(&mut self, xs: &[usize]) {
        self.write_u64(xs.len() as u64);
        for &x in xs {
            self.write_u64(x as u64);
        }
    }

    fn finish(self) -> u64 {
        self.0
    }
}

fn hash_ew_step(h: &mut FnvHasher, step: &FusedEwStep) {
    match *step {
        FusedEwStep::Add(a, b) => {
            h.write_u8(1);
            h.write_u64(a as u64);
            h.write_u64(b as u64);
        }
        FusedEwStep::Sub(a, b) => {
            h.write_u8(2);
            h.write_u64(a as u64);
            h.write_u64(b as u64);
        }
        FusedEwStep::Mul(a, b) => {
            h.write_u8(3);
            h.write_u64(a as u64);
            h.write_u64(b as u64);
        }
        FusedEwStep::Div(a, b) => {
            h.write_u8(4);
            h.write_u64(a as u64);
            h.write_u64(b as u64);
        }
        FusedEwStep::Pow(a, b) => {
            h.write_u8(5);
            h.write_u64(a as u64);
            h.write_u64(b as u64);
        }
        FusedEwStep::AddScalar(a, c) => {
            h.write_u8(6);
            h.write_u64(a as u64);
            h.write_u64(c.to_bits());
        }
        FusedEwStep::SubScalar(a, c, r) => {
            h.write_u8(7);
            h.write_u64(a as u64);
            h.write_u64(c.to_bits());
            h.write_u8(u8::from(r));
        }
        FusedEwStep::MulScalar(a, c) => {
            h.write_u8(8);
            h.write_u64(a as u64);
            h.write_u64(c.to_bits());
        }
        FusedEwStep::DivScalar(a, c, r) => {
            h.write_u8(9);
            h.write_u64(a as u64);
            h.write_u64(c.to_bits());
            h.write_u8(u8::from(r));
        }
        FusedEwStep::PowScalar(a, c, r) => {
            h.write_u8(10);
            h.write_u64(a as u64);
            h.write_u64(c.to_bits());
            h.write_u8(u8::from(r));
        }
        FusedEwStep::Neg(a) => {
            h.write_u8(11);
            h.write_u64(a as u64);
        }
        FusedEwStep::Relu(a) => {
            h.write_u8(12);
            h.write_u64(a as u64);
        }
        FusedEwStep::Gelu(a) => {
            h.write_u8(13);
            h.write_u64(a as u64);
        }
        FusedEwStep::Silu(a) => {
            h.write_u8(14);
            h.write_u64(a as u64);
        }
        FusedEwStep::Tanh(a) => {
            h.write_u8(15);
            h.write_u64(a as u64);
        }
        FusedEwStep::Sigmoid(a) => {
            h.write_u8(16);
            h.write_u64(a as u64);
        }
        FusedEwStep::Exp(a) => {
            h.write_u8(17);
            h.write_u64(a as u64);
        }
        FusedEwStep::Ln(a) => {
            h.write_u8(18);
            h.write_u64(a as u64);
        }
        FusedEwStep::Sqrt(a) => {
            h.write_u8(19);
            h.write_u64(a as u64);
        }
        FusedEwStep::Sin(a) => {
            h.write_u8(20);
            h.write_u64(a as u64);
        }
        FusedEwStep::Cos(a) => {
            h.write_u8(21);
            h.write_u64(a as u64);
        }
        FusedEwStep::Abs(a) => {
            h.write_u8(22);
            h.write_u64(a as u64);
        }
    }
}

// ════════════════════════════════════════════════════════════════════════════
// 6. IR-Level Symbolic Differentiation (VJP / Grad / JVP) on `TensorComputeDag`
// ════════════════════════════════════════════════════════════════════════════

fn broadcast_shapes_dag(a: &[usize], b: &[usize]) -> MoraResult<Vec<usize>> {
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
                "ComputeDag AD: incompatible broadcast shapes {a:?} and {b:?}"
            )));
        }
    }
    Ok(out)
}

fn matmul_shape_dag(a: &[usize], b: &[usize]) -> MoraResult<Vec<usize>> {
    match (a.len(), b.len()) {
        (1, 1) => Ok(Vec::new()),
        (2, 1) => Ok(vec![a[0]]),
        (1, 2) => Ok(vec![b[1]]),
        (ra, rb) if ra >= 2 && rb >= 2 => {
            let mut out = broadcast_shapes_dag(&a[..ra - 2], &b[..rb - 2])?;
            out.push(a[ra - 2]);
            out.push(b[rb - 1]);
            Ok(out)
        }
        _ => Err(MoraError::Runtime(format!(
            "ComputeDag AD: unsupported matmul shapes {a:?} @ {b:?}"
        ))),
    }
}

fn sum_axis_shape_dag(shape: &[usize], axis: usize, keepdim: bool) -> Vec<usize> {
    if keepdim {
        let mut out = shape.to_vec();
        if axis < out.len() {
            out[axis] = 1;
        }
        out
    } else {
        shape
            .iter()
            .enumerate()
            .filter_map(|(i, &d)| if i == axis { None } else { Some(d) })
            .collect()
    }
}

struct DagIrBuilder {
    nodes: Vec<DagNode>,
}

impl DagIrBuilder {
    fn new() -> Self {
        Self { nodes: Vec::new() }
    }

    fn from_nodes(nodes: Vec<DagNode>) -> Self {
        Self { nodes }
    }

    fn push(&mut self, op: DagOp, shape: Vec<usize>, dtype: DType, device: Device) -> usize {
        let id = self.nodes.len();
        self.nodes.push(DagNode {
            op,
            shape,
            dtype,
            device,
        });
        id
    }

    fn shape(&self, id: usize) -> Vec<usize> {
        self.nodes[id].shape.clone()
    }

    fn dtype(&self, id: usize) -> DType {
        self.nodes[id].dtype
    }

    fn device(&self, id: usize) -> Device {
        self.nodes[id].device.clone()
    }

    fn const_filled(
        &mut self,
        val: f64,
        shape: &[usize],
        dtype: DType,
        device: &Device,
    ) -> MoraResult<usize> {
        let n = checked_numel(shape)?;
        let t = Tensor::from_raw(vec![val; n], shape.to_vec(), dtype, device.clone(), false)?;
        Ok(self.push(DagOp::ConstTensor(t), shape.to_vec(), dtype, device.clone()))
    }

    fn add(&mut self, a: usize, b: usize) -> MoraResult<usize> {
        let sh = broadcast_shapes_dag(&self.nodes[a].shape, &self.nodes[b].shape)?;
        let dt = self.dtype(a);
        let dev = self.device(a);
        Ok(self.push(DagOp::Add(a, b), sh, dt, dev))
    }

    fn sub(&mut self, a: usize, b: usize) -> MoraResult<usize> {
        let sh = broadcast_shapes_dag(&self.nodes[a].shape, &self.nodes[b].shape)?;
        let dt = self.dtype(a);
        let dev = self.device(a);
        Ok(self.push(DagOp::Sub(a, b), sh, dt, dev))
    }

    fn mul(&mut self, a: usize, b: usize) -> MoraResult<usize> {
        let sh = broadcast_shapes_dag(&self.nodes[a].shape, &self.nodes[b].shape)?;
        let dt = self.dtype(a);
        let dev = self.device(a);
        Ok(self.push(DagOp::Mul(a, b), sh, dt, dev))
    }

    fn div(&mut self, a: usize, b: usize) -> MoraResult<usize> {
        let sh = broadcast_shapes_dag(&self.nodes[a].shape, &self.nodes[b].shape)?;
        let dt = self.dtype(a);
        let dev = self.device(a);
        Ok(self.push(DagOp::Div(a, b), sh, dt, dev))
    }

    fn pow(&mut self, a: usize, b: usize) -> MoraResult<usize> {
        let sh = broadcast_shapes_dag(&self.nodes[a].shape, &self.nodes[b].shape)?;
        let dt = self.dtype(a);
        let dev = self.device(a);
        Ok(self.push(DagOp::Pow(a, b), sh, dt, dev))
    }

    fn unary(&mut self, op: DagOp, src: usize) -> usize {
        let sh = self.shape(src);
        let dt = self.dtype(src);
        let dev = self.device(src);
        self.push(op, sh, dt, dev)
    }

    fn matmul(&mut self, a: usize, b: usize) -> MoraResult<usize> {
        let sh = matmul_shape_dag(&self.nodes[a].shape, &self.nodes[b].shape)?;
        let dt = self.dtype(a);
        let dev = self.device(a);
        Ok(self.push(DagOp::MatMul(a, b), sh, dt, dev))
    }

    fn transpose(&mut self, a: usize, d0: usize, d1: usize) -> usize {
        let mut sh = self.shape(a);
        sh.swap(d0, d1);
        let dt = self.dtype(a);
        let dev = self.device(a);
        self.push(DagOp::Transpose(a, d0, d1), sh, dt, dev)
    }

    fn reshape(&mut self, a: usize, target: Vec<usize>) -> usize {
        let dt = self.dtype(a);
        let dev = self.device(a);
        self.push(DagOp::Reshape(a, target.clone()), target, dt, dev)
    }

    fn sum_axis(&mut self, a: usize, axis: usize, keepdim: bool) -> usize {
        let sh = sum_axis_shape_dag(&self.nodes[a].shape, axis, keepdim);
        let dt = self.dtype(a);
        let dev = self.device(a);
        self.push(DagOp::SumAxis(a, axis, keepdim), sh, dt, dev)
    }

    fn unbroadcast_to(&mut self, grad_id: usize, target_shape: &[usize]) -> usize {
        let cur_sh = self.shape(grad_id);
        if cur_sh == target_shape {
            return grad_id;
        }
        let dt = self.dtype(grad_id);
        let dev = self.device(grad_id);
        if target_shape.is_empty() {
            return self.push(DagOp::Sum(grad_id), Vec::new(), dt, dev);
        }
        let mut cur = grad_id;
        while self.nodes[cur].shape.len() > target_shape.len() {
            cur = self.sum_axis(cur, 0, false);
        }
        for (ax, &tdim) in target_shape.iter().enumerate() {
            if tdim == 1 && self.nodes[cur].shape[ax] > 1 {
                cur = self.sum_axis(cur, ax, true);
            }
        }
        if self.nodes[cur].shape != target_shape {
            cur = self.reshape(cur, target_shape.to_vec());
        }
        cur
    }

    fn broadcast_to_shape(&mut self, val_id: usize, target_shape: &[usize]) -> MoraResult<usize> {
        if self.nodes[val_id].shape == target_shape {
            return Ok(val_id);
        }
        let dt = self.dtype(val_id);
        let dev = self.device(val_id);
        let zeros = self.const_filled(0.0, target_shape, dt, &dev)?;
        self.add(val_id, zeros)
    }

    fn gelu_derivative(&mut self, a: usize) -> MoraResult<usize> {
        // Exact derivative of tanh-approx GELU:
        //   u = k * (a + 0.044715 * a^3), t = tanh(u)
        //   d/da = 0.5*(1+t) + 0.5*a*(1-t^2)*k*(1 + 3*0.044715*a^2)
        let k = (2.0_f64 / std::f64::consts::PI).sqrt();
        let a2 = self.mul(a, a)?;
        let a3 = self.mul(a2, a)?;
        let c_a3 = self.unary(DagOp::MulScalar(a3, 0.044715), a3);
        let inner = self.add(a, c_a3)?;
        let u = self.unary(DagOp::MulScalar(inner, k), inner);
        let t = self.unary(DagOp::Tanh(u), u);
        let one_plus_t = self.unary(DagOp::AddScalar(t, 1.0), t);
        let term1 = self.unary(DagOp::MulScalar(one_plus_t, 0.5), one_plus_t);
        let t2 = self.mul(t, t)?;
        let sech2 = self.unary(DagOp::SubScalar(t2, 1.0, true), t2);
        let three_c_a2 = self.unary(DagOp::MulScalar(a2, 3.0 * 0.044715), a2);
        let one_plus = self.unary(DagOp::AddScalar(three_c_a2, 1.0), three_c_a2);
        let du = self.unary(DagOp::MulScalar(one_plus, k), one_plus);
        let half_a = self.unary(DagOp::MulScalar(a, 0.5), a);
        let prod1 = self.mul(half_a, sech2)?;
        let term2 = self.mul(prod1, du)?;
        self.add(term1, term2)
    }

    fn silu_derivative(&mut self, a: usize, y: usize) -> MoraResult<usize> {
        // d/da (a * sigmoid(a)) = sigmoid(a) + y * (1 - sigmoid(a))
        let s = self.unary(DagOp::Sigmoid(a), a);
        let one_minus_s = self.unary(DagOp::SubScalar(s, 1.0, true), s);
        let y_oms = self.mul(y, one_minus_s)?;
        self.add(s, y_oms)
    }

    fn apply_fused_act(&mut self, x: usize, act: FusedAct) -> usize {
        match act {
            FusedAct::None => x,
            FusedAct::Relu => self.unary(DagOp::Relu(x), x),
            FusedAct::Gelu => self.unary(DagOp::Gelu(x), x),
            FusedAct::Silu => self.unary(DagOp::Silu(x), x),
            FusedAct::Tanh => self.unary(DagOp::Tanh(x), x),
            FusedAct::Sigmoid => self.unary(DagOp::Sigmoid(x), x),
        }
    }
}

impl TensorComputeDag {
    /// Lower any fused nodes (`FusedLinearAct`, `FusedRmsNormLinear`, `FusedFlashAttention`,
    /// `FusedElementwise`) back into primitive `DagOp` nodes so IR-level symbolic AD
    /// (`build_vjp`, `build_grad`, `build_jvp`) handles both unfused and fused graphs uniformly.
    pub(crate) fn unfuse_to_primitives(&self) -> MoraResult<Self> {
        if !self.nodes.iter().any(|n| n.op.is_fused()) {
            return Ok(self.clone());
        }
        let mut b = DagIrBuilder::new();
        let mut map = vec![0usize; self.nodes.len()];
        for (idx, node) in self.nodes.iter().enumerate() {
            let new_id = match &node.op {
                DagOp::FusedLinearAct { x, w, bias, act } => {
                    let mm = b.matmul(map[*x], map[*w])?;
                    let biased = match bias {
                        Some(bi) => b.add(mm, map[*bi])?,
                        None => mm,
                    };
                    b.apply_fused_act(biased, *act)
                }
                DagOp::FusedRmsNormLinear {
                    x,
                    eps,
                    gamma,
                    w,
                    bias,
                    act,
                } => {
                    let mx = map[*x];
                    let rn = b.unary(DagOp::RmsNorm(mx, *eps), mx);
                    let scaled = match gamma {
                        Some(gi) => b.mul(rn, map[*gi])?,
                        None => rn,
                    };
                    let mm = b.matmul(scaled, map[*w])?;
                    let biased = match bias {
                        Some(bi) => b.add(mm, map[*bi])?,
                        None => mm,
                    };
                    b.apply_fused_act(biased, *act)
                }
                DagOp::FusedFlashAttention {
                    q,
                    k,
                    v,
                    mask,
                    scale,
                    k_is_transposed,
                } => {
                    let mq = map[*q];
                    let mk = map[*k];
                    let mv = map[*v];
                    let kt = if *k_is_transposed {
                        mk
                    } else {
                        let kr = b.nodes[mk].shape.len();
                        b.transpose(mk, kr - 2, kr - 1)
                    };
                    let scores = b.matmul(mq, kt)?;
                    let scaled = if (*scale - 1.0).abs() > 0.0 {
                        b.unary(DagOp::MulScalar(scores, *scale), scores)
                    } else {
                        scores
                    };
                    let logits = match mask {
                        Some(mi) => b.add(scaled, map[*mi])?,
                        None => scaled,
                    };
                    let probs = b.unary(DagOp::Softmax(logits), logits);
                    b.matmul(probs, mv)?
                }
                DagOp::FusedElementwise { inputs, steps } => {
                    let mut slots: Vec<usize> = inputs.iter().map(|&i| map[i]).collect();
                    for step in steps {
                        let step_id = match *step {
                            FusedEwStep::Add(s0, s1) => b.add(slots[s0], slots[s1])?,
                            FusedEwStep::Sub(s0, s1) => b.sub(slots[s0], slots[s1])?,
                            FusedEwStep::Mul(s0, s1) => b.mul(slots[s0], slots[s1])?,
                            FusedEwStep::Div(s0, s1) => b.div(slots[s0], slots[s1])?,
                            FusedEwStep::Pow(s0, s1) => b.pow(slots[s0], slots[s1])?,
                            FusedEwStep::AddScalar(s0, c) => {
                                b.unary(DagOp::AddScalar(slots[s0], c), slots[s0])
                            }
                            FusedEwStep::SubScalar(s0, c, r) => {
                                b.unary(DagOp::SubScalar(slots[s0], c, r), slots[s0])
                            }
                            FusedEwStep::MulScalar(s0, c) => {
                                b.unary(DagOp::MulScalar(slots[s0], c), slots[s0])
                            }
                            FusedEwStep::DivScalar(s0, c, r) => {
                                b.unary(DagOp::DivScalar(slots[s0], c, r), slots[s0])
                            }
                            FusedEwStep::PowScalar(s0, c, r) => {
                                b.unary(DagOp::PowScalar(slots[s0], c, r), slots[s0])
                            }
                            FusedEwStep::Neg(s0) => b.unary(DagOp::Neg(slots[s0]), slots[s0]),
                            FusedEwStep::Relu(s0) => b.unary(DagOp::Relu(slots[s0]), slots[s0]),
                            FusedEwStep::Gelu(s0) => b.unary(DagOp::Gelu(slots[s0]), slots[s0]),
                            FusedEwStep::Silu(s0) => b.unary(DagOp::Silu(slots[s0]), slots[s0]),
                            FusedEwStep::Tanh(s0) => b.unary(DagOp::Tanh(slots[s0]), slots[s0]),
                            FusedEwStep::Sigmoid(s0) => {
                                b.unary(DagOp::Sigmoid(slots[s0]), slots[s0])
                            }
                            FusedEwStep::Exp(s0) => b.unary(DagOp::Exp(slots[s0]), slots[s0]),
                            FusedEwStep::Ln(s0) => b.unary(DagOp::Ln(slots[s0]), slots[s0]),
                            FusedEwStep::Sqrt(s0) => b.unary(DagOp::Sqrt(slots[s0]), slots[s0]),
                            FusedEwStep::Sin(s0) => b.unary(DagOp::Sin(slots[s0]), slots[s0]),
                            FusedEwStep::Cos(s0) => b.unary(DagOp::Cos(slots[s0]), slots[s0]),
                            FusedEwStep::Abs(s0) => b.unary(DagOp::Abs(slots[s0]), slots[s0]),
                        };
                        slots.push(step_id);
                    }
                    *slots.last().ok_or_else(|| {
                        MoraError::Runtime("ComputeDag unfuse: empty FusedElementwise".to_string())
                    })?
                }
                _ => {
                    let mut cloned = node.clone();
                    cloned.op.remap_inputs(&map);
                    b.push(cloned.op, cloned.shape, cloned.dtype, cloned.device)
                }
            };
            map[idx] = new_id;
        }
        Ok(Self {
            input_specs: self.input_specs.clone(),
            nodes: b.nodes,
            output: map[self.output],
            fused: false,
            algebraic_rewrites: 0,
            compiled_plan: None,
        })
    }

    fn build_vjp_internal(&self, input_idx: usize, seed_ones: bool) -> MoraResult<Self> {
        if input_idx >= self.input_specs.len() {
            return Err(MoraError::Runtime(format!(
                "ComputeDag.{}: input index {input_idx} out of bounds (graph has {} input(s))",
                if seed_ones { "grad" } else { "vjp" },
                self.input_specs.len()
            )));
        }
        let prim = self.unfuse_to_primitives()?;
        let n_fwd = prim.nodes.len();
        let out_node = &prim.nodes[prim.output];
        let out_shape = out_node.shape.clone();
        let out_dtype = out_node.dtype;
        let out_dev = out_node.device.clone();

        let mut b = DagIrBuilder::from_nodes(prim.nodes.clone());
        let mut new_input_specs = prim.input_specs.clone();

        let seed_id = if seed_ones {
            b.const_filled(1.0, &out_shape, out_dtype, &out_dev)?
        } else {
            let slot = new_input_specs.len();
            new_input_specs.push((out_shape.clone(), out_dtype, out_dev.clone()));
            b.push(DagOp::Input(slot), out_shape, out_dtype, out_dev)
        };

        let mut adj: Vec<Option<usize>> = vec![None; n_fwd];
        adj[prim.output] = Some(seed_id);

        let acc = |adj: &mut [Option<usize>],
                   target: usize,
                   contrib: usize,
                   builder: &mut DagIrBuilder|
         -> MoraResult<()> {
            if let Some(prev) = adj[target] {
                adj[target] = Some(builder.add(prev, contrib)?);
            } else {
                adj[target] = Some(contrib);
            }
            Ok(())
        };

        for idx in (0..n_fwd).rev() {
            let Some(g) = adj[idx] else {
                continue;
            };
            let node_op = prim.nodes[idx].op.clone();
            match node_op {
                DagOp::Input(_) | DagOp::ConstTensor(_) | DagOp::StepPos(_) | DagOp::Sign(_) => {}
                DagOp::Identity(a) => acc(&mut adj, a, g, &mut b)?,
                DagOp::Reshape(a, _) => {
                    let sh_a = b.shape(a);
                    let r = b.reshape(g, sh_a);
                    acc(&mut adj, a, r, &mut b)?;
                }
                DagOp::Transpose(a, d0, d1) => {
                    let t = b.transpose(g, d0, d1);
                    acc(&mut adj, a, t, &mut b)?;
                }
                DagOp::Add(a, rhs) => {
                    let sa = b.shape(a);
                    let sb = b.shape(rhs);
                    let da = b.unbroadcast_to(g, &sa);
                    let db = b.unbroadcast_to(g, &sb);
                    acc(&mut adj, a, da, &mut b)?;
                    acc(&mut adj, rhs, db, &mut b)?;
                }
                DagOp::Sub(a, rhs) => {
                    let sa = b.shape(a);
                    let sb = b.shape(rhs);
                    let da = b.unbroadcast_to(g, &sa);
                    let ng = b.unary(DagOp::Neg(g), g);
                    let db = b.unbroadcast_to(ng, &sb);
                    acc(&mut adj, a, da, &mut b)?;
                    acc(&mut adj, rhs, db, &mut b)?;
                }
                DagOp::Mul(a, rhs) => {
                    let sa = b.shape(a);
                    let sb = b.shape(rhs);
                    let ga = b.mul(g, rhs)?;
                    let da = b.unbroadcast_to(ga, &sa);
                    let gb = b.mul(g, a)?;
                    let db = b.unbroadcast_to(gb, &sb);
                    acc(&mut adj, a, da, &mut b)?;
                    acc(&mut adj, rhs, db, &mut b)?;
                }
                DagOp::Div(a, rhs) => {
                    let sa = b.shape(a);
                    let sb = b.shape(rhs);
                    let ga = b.div(g, rhs)?;
                    let da = b.unbroadcast_to(ga, &sa);
                    let num = b.mul(g, a)?;
                    let den = b.mul(rhs, rhs)?;
                    let frac = b.div(num, den)?;
                    let neg_frac = b.unary(DagOp::Neg(frac), frac);
                    let db = b.unbroadcast_to(neg_frac, &sb);
                    acc(&mut adj, a, da, &mut b)?;
                    acc(&mut adj, rhs, db, &mut b)?;
                }
                DagOp::Pow(a, rhs) => {
                    let sa = b.shape(a);
                    let sb = b.shape(rhs);
                    let bm1 = b.unary(DagOp::SubScalar(rhs, 1.0, false), rhs);
                    let pow_bm1 = b.pow(a, bm1)?;
                    let b_pow = b.mul(rhs, pow_bm1)?;
                    let ga = b.mul(g, b_pow)?;
                    let da = b.unbroadcast_to(ga, &sa);
                    let ln_a = b.unary(DagOp::Ln(a), a);
                    let y_lna = b.mul(idx, ln_a)?;
                    let gb = b.mul(g, y_lna)?;
                    let db = b.unbroadcast_to(gb, &sb);
                    acc(&mut adj, a, da, &mut b)?;
                    acc(&mut adj, rhs, db, &mut b)?;
                }
                DagOp::AddScalar(a, _) => acc(&mut adj, a, g, &mut b)?,
                DagOp::SubScalar(a, _, rev) => {
                    let da = if rev { b.unary(DagOp::Neg(g), g) } else { g };
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::MulScalar(a, c) => {
                    let da = b.unary(DagOp::MulScalar(g, c), g);
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::DivScalar(a, c, rev) => {
                    let da = if !rev {
                        b.unary(DagOp::DivScalar(g, c, false), g)
                    } else {
                        let a2 = b.mul(a, a)?;
                        let div = b.div(g, a2)?;
                        b.unary(DagOp::MulScalar(div, -c), div)
                    };
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::PowScalar(a, c, rev) => {
                    if !rev {
                        if c == 1.0 {
                            acc(&mut adj, a, g, &mut b)?;
                        } else if c == 2.0 {
                            let two_a = b.unary(DagOp::MulScalar(a, 2.0), a);
                            let da = b.mul(g, two_a)?;
                            acc(&mut adj, a, da, &mut b)?;
                        } else if c != 0.0 {
                            let p = b.unary(DagOp::PowScalar(a, c - 1.0, false), a);
                            let cp = b.unary(DagOp::MulScalar(p, c), p);
                            let da = b.mul(g, cp)?;
                            acc(&mut adj, a, da, &mut b)?;
                        }
                    } else if c > 0.0 {
                        let scaled = b.unary(DagOp::MulScalar(idx, c.ln()), idx);
                        let da = b.mul(g, scaled)?;
                        acc(&mut adj, a, da, &mut b)?;
                    }
                }
                DagOp::Neg(a) => {
                    let ng = b.unary(DagOp::Neg(g), g);
                    acc(&mut adj, a, ng, &mut b)?;
                }
                DagOp::Exp(a) => {
                    let da = b.mul(g, idx)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Ln(a) => {
                    let da = b.div(g, a)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Sqrt(a) => {
                    let two_y = b.unary(DagOp::MulScalar(idx, 2.0), idx);
                    let da = b.div(g, two_y)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Sin(a) => {
                    let ca = b.unary(DagOp::Cos(a), a);
                    let da = b.mul(g, ca)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Cos(a) => {
                    let sa = b.unary(DagOp::Sin(a), a);
                    let prod = b.mul(g, sa)?;
                    let da = b.unary(DagOp::Neg(prod), prod);
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Tanh(a) => {
                    let y2 = b.mul(idx, idx)?;
                    let one_m_y2 = b.unary(DagOp::SubScalar(y2, 1.0, true), y2);
                    let da = b.mul(g, one_m_y2)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Sigmoid(a) => {
                    let oms = b.unary(DagOp::SubScalar(idx, 1.0, true), idx);
                    let deriv = b.mul(idx, oms)?;
                    let da = b.mul(g, deriv)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Silu(a) => {
                    let deriv = b.silu_derivative(a, idx)?;
                    let da = b.mul(g, deriv)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Gelu(a) => {
                    let deriv = b.gelu_derivative(a)?;
                    let da = b.mul(g, deriv)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Relu(a) => {
                    let mask = b.unary(DagOp::StepPos(a), a);
                    let da = b.mul(g, mask)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Abs(a) => {
                    let sgn = b.unary(DagOp::Sign(a), a);
                    let da = b.mul(g, sgn)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Sum(a) => {
                    let sa = b.shape(a);
                    let dt = b.dtype(a);
                    let dev = b.device(a);
                    let ones = b.const_filled(1.0, &sa, dt, &dev)?;
                    let da = b.mul(ones, g)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Mean(a) => {
                    let sa = b.shape(a);
                    let n = checked_numel(&sa)?.max(1) as f64;
                    let dt = b.dtype(a);
                    let dev = b.device(a);
                    let scaled_g = b.unary(DagOp::DivScalar(g, n, false), g);
                    let ones = b.const_filled(1.0, &sa, dt, &dev)?;
                    let da = b.mul(ones, scaled_g)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::SumAxis(a, axis, keepdim) => {
                    let sa = b.shape(a);
                    let dt = b.dtype(a);
                    let dev = b.device(a);
                    let g_kd = if keepdim {
                        g
                    } else {
                        let mut kd_sh = b.shape(g);
                        kd_sh.insert(axis.min(kd_sh.len()), 1);
                        b.reshape(g, kd_sh)
                    };
                    let ones = b.const_filled(1.0, &sa, dt, &dev)?;
                    let da = b.mul(ones, g_kd)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::Gather(a, indices) => {
                    let sa = b.shape(a);
                    let out_n = checked_numel(&sa)?;
                    let dt = b.dtype(a);
                    let dev = b.device(a);
                    let sc = b.push(DagOp::ScatterAdd(g, out_n, indices), sa, dt, dev);
                    acc(&mut adj, a, sc, &mut b)?;
                }
                DagOp::ScatterAdd(a, _, indices) => {
                    let sa = b.shape(a);
                    let dt = b.dtype(a);
                    let dev = b.device(a);
                    let ga = b.push(DagOp::Gather(g, indices), sa, dt, dev);
                    acc(&mut adj, a, ga, &mut b)?;
                }
                DagOp::MatMul(a, rhs) => {
                    let sa = b.shape(a);
                    let sb = b.shape(rhs);
                    match (sa.len(), sb.len()) {
                        (ra, rb) if ra >= 2 && rb >= 2 => {
                            let bt = b.transpose(rhs, rb - 2, rb - 1);
                            let da_raw = b.matmul(g, bt)?;
                            let da = b.unbroadcast_to(da_raw, &sa);
                            let at = b.transpose(a, ra - 2, ra - 1);
                            let db_raw = b.matmul(at, g)?;
                            let db = b.unbroadcast_to(db_raw, &sb);
                            acc(&mut adj, a, da, &mut b)?;
                            acc(&mut adj, rhs, db, &mut b)?;
                        }
                        (1, 1) => {
                            let da = b.mul(rhs, g)?;
                            let db = b.mul(a, g)?;
                            acc(&mut adj, a, da, &mut b)?;
                            acc(&mut adj, rhs, db, &mut b)?;
                        }
                        (2, 1) => {
                            let g_col = b.reshape(g, vec![sa[0], 1]);
                            let b_row = b.reshape(rhs, vec![1, sb[0]]);
                            let da = b.matmul(g_col, b_row)?;
                            let at = b.transpose(a, 0, 1);
                            let db_col = b.matmul(at, g_col)?;
                            let db = b.reshape(db_col, sb);
                            acc(&mut adj, a, da, &mut b)?;
                            acc(&mut adj, rhs, db, &mut b)?;
                        }
                        (1, 2) => {
                            let g_row = b.reshape(g, vec![1, sb[1]]);
                            let bt = b.transpose(rhs, 0, 1);
                            let da_row = b.matmul(g_row, bt)?;
                            let da = b.reshape(da_row, sa.clone());
                            let a_col = b.reshape(a, vec![sa[0], 1]);
                            let db = b.matmul(a_col, g_row)?;
                            acc(&mut adj, a, da, &mut b)?;
                            acc(&mut adj, rhs, db, &mut b)?;
                        }
                        _ => {
                            return Err(MoraError::Runtime(format!(
                                "ComputeDag VJP: unsupported MatMul shapes {sa:?} @ {sb:?}"
                            )));
                        }
                    }
                }
                DagOp::Softmax(a) => {
                    let rank = b.nodes[idx].shape.len();
                    let last_ax = rank.saturating_sub(1);
                    let gy = b.mul(g, idx)?;
                    let dot = if rank == 0 {
                        gy
                    } else {
                        b.sum_axis(gy, last_ax, true)
                    };
                    let diff = b.sub(g, dot)?;
                    let da = b.mul(idx, diff)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::RmsNorm(a, eps) => {
                    let sa = b.shape(a);
                    let rank = sa.len();
                    let last_ax = rank.saturating_sub(1);
                    let d = sa.last().copied().unwrap_or(1).max(1) as f64;
                    let a2 = b.mul(a, a)?;
                    let sum_sq = if rank == 0 {
                        a2
                    } else {
                        b.sum_axis(a2, last_ax, true)
                    };
                    let ms = b.unary(DagOp::MulScalar(sum_sq, 1.0 / d), sum_sq);
                    let ms_eps = b.unary(DagOp::AddScalar(ms, eps), ms);
                    let rms = b.unary(DagOp::Sqrt(ms_eps), ms_eps);
                    let inv_rms = b.unary(DagOp::DivScalar(rms, 1.0, true), rms);
                    let gy = b.mul(g, idx)?;
                    let dot = if rank == 0 {
                        gy
                    } else {
                        b.sum_axis(gy, last_ax, true)
                    };
                    let mean_gy = b.unary(DagOp::MulScalar(dot, 1.0 / d), dot);
                    let proj = b.mul(idx, mean_gy)?;
                    let sub = b.sub(g, proj)?;
                    let da = b.mul(inv_rms, sub)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::LayerNorm(a, eps) => {
                    let sa = b.shape(a);
                    let rank = sa.len();
                    let last_ax = rank.saturating_sub(1);
                    let d = sa.last().copied().unwrap_or(1).max(1) as f64;
                    let sum_a = if rank == 0 {
                        a
                    } else {
                        b.sum_axis(a, last_ax, true)
                    };
                    let mean = b.unary(DagOp::MulScalar(sum_a, 1.0 / d), sum_a);
                    let centered = b.sub(a, mean)?;
                    let c2 = b.mul(centered, centered)?;
                    let sum_c2 = if rank == 0 {
                        c2
                    } else {
                        b.sum_axis(c2, last_ax, true)
                    };
                    let var = b.unary(DagOp::MulScalar(sum_c2, 1.0 / d), sum_c2);
                    let var_eps = b.unary(DagOp::AddScalar(var, eps), var);
                    let std = b.unary(DagOp::Sqrt(var_eps), var_eps);
                    let inv_std = b.unary(DagOp::DivScalar(std, 1.0, true), std);
                    let sum_g = if rank == 0 {
                        g
                    } else {
                        b.sum_axis(g, last_ax, true)
                    };
                    let mean_g = b.unary(DagOp::MulScalar(sum_g, 1.0 / d), sum_g);
                    let gy = b.mul(g, idx)?;
                    let sum_gy = if rank == 0 {
                        gy
                    } else {
                        b.sum_axis(gy, last_ax, true)
                    };
                    let mean_gy = b.unary(DagOp::MulScalar(sum_gy, 1.0 / d), sum_gy);
                    let g_centered = b.sub(g, mean_g)?;
                    let y_term = b.mul(idx, mean_gy)?;
                    let diff = b.sub(g_centered, y_term)?;
                    let da = b.mul(inv_std, diff)?;
                    acc(&mut adj, a, da, &mut b)?;
                }
                DagOp::FusedLinearAct { .. }
                | DagOp::FusedRmsNormLinear { .. }
                | DagOp::FusedFlashAttention { .. }
                | DagOp::FusedElementwise { .. } => {
                    unreachable!("unfuse_to_primitives removes all fused nodes")
                }
            }
        }

        // Accumulate adjoints across all `DagOp::Input(input_idx)` occurrences (handles graphs that
        // reference the same input slot across multiple nodes or return a constant independent of it).
        let mut grad_out: Option<usize> = None;
        for (idx, node) in prim.nodes.iter().enumerate() {
            if matches!(node.op, DagOp::Input(s) if s == input_idx)
                && let Some(g) = adj[idx]
            {
                grad_out = match grad_out {
                    None => Some(g),
                    Some(prev) => Some(b.add(prev, g)?),
                };
            }
        }
        let raw_out = match grad_out {
            Some(id) => id,
            None => {
                let (ref sh, dt, ref dev) = prim.input_specs[input_idx];
                b.const_filled(0.0, sh, dt, dev)?
            }
        };
        let (nodes, output) = compact_live_nodes(b.nodes, raw_out);
        Ok(Self {
            input_specs: new_input_specs,
            nodes,
            output,
            fused: false,
            algebraic_rewrites: 0,
            compiled_plan: None,
        })
    }

    /// Build a first-class symbolic reverse-mode VJP [`TensorComputeDag`] with respect to
    /// `input_idx`. The returned DAG takes `(x_0, ..., x_{N-1}, dy)` and computes `J_f(x)^T dy`.
    pub(crate) fn build_vjp(&self, input_idx: usize) -> MoraResult<Self> {
        self.build_vjp_internal(input_idx, false)
    }

    /// Build a first-class symbolic reverse-mode gradient [`TensorComputeDag`] with respect to
    /// `input_idx` (seeding output cotangent `dy = ones_like(output)`). The returned DAG takes
    /// `(x_0, ..., x_{N-1})` and computes `d(sum(f(x))) / dx_{input_idx}`.
    pub(crate) fn build_grad(&self, input_idx: usize) -> MoraResult<Self> {
        self.build_vjp_internal(input_idx, true)
    }

    /// Build a first-class symbolic forward-mode JVP [`TensorComputeDag`] with respect to
    /// `input_idx`. The returned DAG takes `(x_0, ..., x_{N-1}, dx)` and computes `J_f(x) dx`.
    pub(crate) fn build_jvp(&self, input_idx: usize) -> MoraResult<Self> {
        if input_idx >= self.input_specs.len() {
            return Err(MoraError::Runtime(format!(
                "ComputeDag.jvp: input index {input_idx} out of bounds (graph has {} input(s))",
                self.input_specs.len()
            )));
        }
        let prim = self.unfuse_to_primitives()?;
        let n_fwd = prim.nodes.len();
        let mut b = DagIrBuilder::from_nodes(prim.nodes.clone());
        let mut new_input_specs = prim.input_specs.clone();
        let (tan_sh, tan_dt, tan_dev) = prim.input_specs[input_idx].clone();
        let tan_slot = new_input_specs.len();
        new_input_specs.push((tan_sh.clone(), tan_dt, tan_dev.clone()));
        let dx_node = b.push(DagOp::Input(tan_slot), tan_sh, tan_dt, tan_dev);

        let mut tan: Vec<Option<usize>> = vec![None; n_fwd];
        for (idx, node) in prim.nodes.iter().enumerate() {
            let y_sh = &node.shape;
            tan[idx] = match &node.op {
                DagOp::Input(s) => {
                    if *s == input_idx {
                        Some(dx_node)
                    } else {
                        None
                    }
                }
                DagOp::ConstTensor(_) | DagOp::StepPos(_) | DagOp::Sign(_) => None,
                DagOp::Identity(a) => tan[*a],
                DagOp::Reshape(a, sh) => tan[*a].map(|ta| b.reshape(ta, sh.clone())),
                DagOp::Transpose(a, d0, d1) => tan[*a].map(|ta| b.transpose(ta, *d0, *d1)),
                DagOp::Add(a, rhs) => match (tan[*a], tan[*rhs]) {
                    (Some(ta), Some(tb)) => Some(b.add(ta, tb)?),
                    (Some(ta), None) => Some(b.broadcast_to_shape(ta, y_sh)?),
                    (None, Some(tb)) => Some(b.broadcast_to_shape(tb, y_sh)?),
                    (None, None) => None,
                },
                DagOp::Sub(a, rhs) => match (tan[*a], tan[*rhs]) {
                    (Some(ta), Some(tb)) => Some(b.sub(ta, tb)?),
                    (Some(ta), None) => Some(b.broadcast_to_shape(ta, y_sh)?),
                    (None, Some(tb)) => {
                        let nb = b.unary(DagOp::Neg(tb), tb);
                        Some(b.broadcast_to_shape(nb, y_sh)?)
                    }
                    (None, None) => None,
                },
                DagOp::Mul(a, rhs) => {
                    let t1 = match tan[*a] {
                        Some(ta) => Some(b.mul(ta, *rhs)?),
                        None => None,
                    };
                    let t2 = match tan[*rhs] {
                        Some(tb) => Some(b.mul(*a, tb)?),
                        None => None,
                    };
                    match (t1, t2) {
                        (Some(x), Some(y)) => Some(b.add(x, y)?),
                        (Some(x), None) | (None, Some(x)) => Some(x),
                        (None, None) => None,
                    }
                }
                DagOp::Div(a, rhs) => {
                    let t1 = match tan[*a] {
                        Some(ta) => Some(b.div(ta, *rhs)?),
                        None => None,
                    };
                    let t2 = match tan[*rhs] {
                        Some(tb) => {
                            let num = b.mul(*a, tb)?;
                            let den = b.mul(*rhs, *rhs)?;
                            Some(b.div(num, den)?)
                        }
                        None => None,
                    };
                    match (t1, t2) {
                        (Some(x), Some(y)) => Some(b.sub(x, y)?),
                        (Some(x), None) => Some(x),
                        (None, Some(y)) => Some(b.unary(DagOp::Neg(y), y)),
                        (None, None) => None,
                    }
                }
                DagOp::Pow(a, rhs) => {
                    let t1 = match tan[*a] {
                        Some(ta) => {
                            let bm1 = b.unary(DagOp::SubScalar(*rhs, 1.0, false), *rhs);
                            let pow_bm1 = b.pow(*a, bm1)?;
                            let factor = b.mul(*rhs, pow_bm1)?;
                            Some(b.mul(ta, factor)?)
                        }
                        None => None,
                    };
                    let t2 = match tan[*rhs] {
                        Some(tb) => {
                            let lna = b.unary(DagOp::Ln(*a), *a);
                            let ylna = b.mul(idx, lna)?;
                            Some(b.mul(tb, ylna)?)
                        }
                        None => None,
                    };
                    match (t1, t2) {
                        (Some(x), Some(y)) => Some(b.add(x, y)?),
                        (Some(x), None) | (None, Some(x)) => Some(x),
                        (None, None) => None,
                    }
                }
                DagOp::AddScalar(a, _) => tan[*a],
                DagOp::SubScalar(a, _, rev) => tan[*a].map(|ta| {
                    if *rev {
                        b.unary(DagOp::Neg(ta), ta)
                    } else {
                        ta
                    }
                }),
                DagOp::MulScalar(a, c) => tan[*a].map(|ta| b.unary(DagOp::MulScalar(ta, *c), ta)),
                DagOp::DivScalar(a, c, rev) => match tan[*a] {
                    None => None,
                    Some(ta) if !rev => Some(b.unary(DagOp::DivScalar(ta, *c, false), ta)),
                    Some(ta) => {
                        let a2 = b.mul(*a, *a)?;
                        let frac = b.div(ta, a2)?;
                        Some(b.unary(DagOp::MulScalar(frac, -*c), frac))
                    }
                },
                DagOp::PowScalar(a, c, rev) => match tan[*a] {
                    None => None,
                    Some(ta) if !rev => {
                        if *c == 1.0 {
                            Some(ta)
                        } else if *c == 2.0 {
                            let two_a = b.unary(DagOp::MulScalar(*a, 2.0), *a);
                            Some(b.mul(ta, two_a)?)
                        } else if *c == 0.0 {
                            None
                        } else {
                            let p = b.unary(DagOp::PowScalar(*a, *c - 1.0, false), *a);
                            let cp = b.unary(DagOp::MulScalar(p, *c), p);
                            Some(b.mul(ta, cp)?)
                        }
                    }
                    Some(ta) => {
                        if *c > 0.0 {
                            let scaled = b.unary(DagOp::MulScalar(idx, c.ln()), idx);
                            Some(b.mul(ta, scaled)?)
                        } else {
                            None
                        }
                    }
                },
                DagOp::Neg(a) => tan[*a].map(|ta| b.unary(DagOp::Neg(ta), ta)),
                DagOp::Exp(a) => match tan[*a] {
                    Some(ta) => Some(b.mul(ta, idx)?),
                    None => None,
                },
                DagOp::Ln(a) => match tan[*a] {
                    Some(ta) => Some(b.div(ta, *a)?),
                    None => None,
                },
                DagOp::Sqrt(a) => match tan[*a] {
                    Some(ta) => {
                        let two_y = b.unary(DagOp::MulScalar(idx, 2.0), idx);
                        Some(b.div(ta, two_y)?)
                    }
                    None => None,
                },
                DagOp::Sin(a) => match tan[*a] {
                    Some(ta) => {
                        let ca = b.unary(DagOp::Cos(*a), *a);
                        Some(b.mul(ta, ca)?)
                    }
                    None => None,
                },
                DagOp::Cos(a) => match tan[*a] {
                    Some(ta) => {
                        let sa = b.unary(DagOp::Sin(*a), *a);
                        let p = b.mul(ta, sa)?;
                        Some(b.unary(DagOp::Neg(p), p))
                    }
                    None => None,
                },
                DagOp::Tanh(a) => match tan[*a] {
                    Some(ta) => {
                        let y2 = b.mul(idx, idx)?;
                        let sech2 = b.unary(DagOp::SubScalar(y2, 1.0, true), y2);
                        Some(b.mul(ta, sech2)?)
                    }
                    None => None,
                },
                DagOp::Sigmoid(a) => match tan[*a] {
                    Some(ta) => {
                        let oms = b.unary(DagOp::SubScalar(idx, 1.0, true), idx);
                        let d = b.mul(idx, oms)?;
                        Some(b.mul(ta, d)?)
                    }
                    None => None,
                },
                DagOp::Silu(a) => match tan[*a] {
                    Some(ta) => {
                        let d = b.silu_derivative(*a, idx)?;
                        Some(b.mul(ta, d)?)
                    }
                    None => None,
                },
                DagOp::Gelu(a) => match tan[*a] {
                    Some(ta) => {
                        let d = b.gelu_derivative(*a)?;
                        Some(b.mul(ta, d)?)
                    }
                    None => None,
                },
                DagOp::Relu(a) => match tan[*a] {
                    Some(ta) => {
                        let m = b.unary(DagOp::StepPos(*a), *a);
                        Some(b.mul(ta, m)?)
                    }
                    None => None,
                },
                DagOp::Abs(a) => match tan[*a] {
                    Some(ta) => {
                        let s = b.unary(DagOp::Sign(*a), *a);
                        Some(b.mul(ta, s)?)
                    }
                    None => None,
                },
                DagOp::MatMul(a, rhs) => {
                    let t1 = match tan[*a] {
                        Some(ta) => Some(b.matmul(ta, *rhs)?),
                        None => None,
                    };
                    let t2 = match tan[*rhs] {
                        Some(tb) => Some(b.matmul(*a, tb)?),
                        None => None,
                    };
                    match (t1, t2) {
                        (Some(x), Some(y)) => Some(b.add(x, y)?),
                        (Some(x), None) | (None, Some(x)) => Some(x),
                        (None, None) => None,
                    }
                }
                DagOp::Sum(a) => tan[*a]
                    .map(|ta| b.push(DagOp::Sum(ta), Vec::new(), node.dtype, node.device.clone())),
                DagOp::Mean(a) => tan[*a]
                    .map(|ta| b.push(DagOp::Mean(ta), Vec::new(), node.dtype, node.device.clone())),
                DagOp::SumAxis(a, ax, kd) => tan[*a].map(|ta| b.sum_axis(ta, *ax, *kd)),
                DagOp::Gather(a, idxs) => tan[*a].map(|ta| {
                    b.push(
                        DagOp::Gather(ta, idxs.clone()),
                        y_sh.clone(),
                        node.dtype,
                        node.device.clone(),
                    )
                }),
                DagOp::ScatterAdd(a, out_n, idxs) => tan[*a].map(|ta| {
                    b.push(
                        DagOp::ScatterAdd(ta, *out_n, idxs.clone()),
                        y_sh.clone(),
                        node.dtype,
                        node.device.clone(),
                    )
                }),
                DagOp::Softmax(a) => match tan[*a] {
                    None => None,
                    Some(ta) => {
                        let rank = y_sh.len();
                        let last_ax = rank.saturating_sub(1);
                        let yta = b.mul(idx, ta)?;
                        let dot = if rank == 0 {
                            yta
                        } else {
                            b.sum_axis(yta, last_ax, true)
                        };
                        let diff = b.sub(ta, dot)?;
                        Some(b.mul(idx, diff)?)
                    }
                },
                DagOp::RmsNorm(a, eps) => match tan[*a] {
                    None => None,
                    Some(ta) => {
                        let sa = b.shape(*a);
                        let rank = sa.len();
                        let last_ax = rank.saturating_sub(1);
                        let d = sa.last().copied().unwrap_or(1).max(1) as f64;
                        let a2 = b.mul(*a, *a)?;
                        let sum_sq = if rank == 0 {
                            a2
                        } else {
                            b.sum_axis(a2, last_ax, true)
                        };
                        let ms = b.unary(DagOp::MulScalar(sum_sq, 1.0 / d), sum_sq);
                        let ms_eps = b.unary(DagOp::AddScalar(ms, *eps), ms);
                        let rms = b.unary(DagOp::Sqrt(ms_eps), ms_eps);
                        let inv_rms = b.unary(DagOp::DivScalar(rms, 1.0, true), rms);
                        let yta = b.mul(idx, ta)?;
                        let dot = if rank == 0 {
                            yta
                        } else {
                            b.sum_axis(yta, last_ax, true)
                        };
                        let mean_yta = b.unary(DagOp::MulScalar(dot, 1.0 / d), dot);
                        let proj = b.mul(idx, mean_yta)?;
                        let diff = b.sub(ta, proj)?;
                        Some(b.mul(inv_rms, diff)?)
                    }
                },
                DagOp::LayerNorm(a, eps) => match tan[*a] {
                    None => None,
                    Some(ta) => {
                        let sa = b.shape(*a);
                        let rank = sa.len();
                        let last_ax = rank.saturating_sub(1);
                        let d = sa.last().copied().unwrap_or(1).max(1) as f64;
                        let sum_a = if rank == 0 {
                            *a
                        } else {
                            b.sum_axis(*a, last_ax, true)
                        };
                        let mean = b.unary(DagOp::MulScalar(sum_a, 1.0 / d), sum_a);
                        let centered = b.sub(*a, mean)?;
                        let c2 = b.mul(centered, centered)?;
                        let sum_c2 = if rank == 0 {
                            c2
                        } else {
                            b.sum_axis(c2, last_ax, true)
                        };
                        let var = b.unary(DagOp::MulScalar(sum_c2, 1.0 / d), sum_c2);
                        let var_eps = b.unary(DagOp::AddScalar(var, *eps), var);
                        let std = b.unary(DagOp::Sqrt(var_eps), var_eps);
                        let inv_std = b.unary(DagOp::DivScalar(std, 1.0, true), std);
                        let sum_ta = if rank == 0 {
                            ta
                        } else {
                            b.sum_axis(ta, last_ax, true)
                        };
                        let mean_ta = b.unary(DagOp::MulScalar(sum_ta, 1.0 / d), sum_ta);
                        let yta = b.mul(idx, ta)?;
                        let sum_yta = if rank == 0 {
                            yta
                        } else {
                            b.sum_axis(yta, last_ax, true)
                        };
                        let mean_yta = b.unary(DagOp::MulScalar(sum_yta, 1.0 / d), sum_yta);
                        let ta_c = b.sub(ta, mean_ta)?;
                        let proj = b.mul(idx, mean_yta)?;
                        let diff = b.sub(ta_c, proj)?;
                        Some(b.mul(inv_std, diff)?)
                    }
                },
                DagOp::FusedLinearAct { .. }
                | DagOp::FusedRmsNormLinear { .. }
                | DagOp::FusedFlashAttention { .. }
                | DagOp::FusedElementwise { .. } => {
                    unreachable!("unfuse_to_primitives removes all fused nodes")
                }
            };
        }

        let raw_out = match tan[prim.output] {
            Some(id) => id,
            None => {
                let out_n = &prim.nodes[prim.output];
                b.const_filled(0.0, &out_n.shape, out_n.dtype, &out_n.device)?
            }
        };
        let (nodes, output) = compact_live_nodes(b.nodes, raw_out);
        Ok(Self {
            input_specs: new_input_specs,
            nodes,
            output,
            fused: false,
            algebraic_rewrites: 0,
            compiled_plan: None,
        })
    }
}
