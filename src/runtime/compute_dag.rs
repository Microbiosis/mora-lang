//! Wave 4 P1: Native `TensorComputeDag`, Deep Operator Fusion, Hardware-SIMD CPU Backend,
//! Compile-Time Buffer Lifetime Reuse (`DagMemoryPlan`), and Structural Graph Cache (`DagPlanCache`).
//!
//! Architecture (`AGENTS.md §0`):
//! - Explicit inspectable compute-graph IR (`DagNode` / `DagOp`) captured from arbitrary Mora
//!   functions (including higher-order `grad(f)` backward passes!) via `trace_dag(f, inputs...)`.
//! - Four-pass operator-fusion pipeline (`TensorComputeDag::fuse`):
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

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::{Mutex, RwLock};

use crate::common::{DType, Device};
use crate::error::{MoraError, MoraResult};
use crate::runtime::autograd::{
    AutogradTape, TapeOp, scatter_add_flat, sum_axis_flat, transpose_flat,
};
use crate::runtime::simd_kernels;
use crate::value::tensor::{Tensor, checked_numel, quantize_for_dtype};

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

        Self {
            node_slots,
            slots,
            total_intermediates,
            buffer_slots,
            naive_peak_bytes,
            planned_peak_bytes,
            reuse_count,
        }
    }
}

/// Reusable physical scratch arena matching a [`DagMemoryPlan`].
#[derive(Debug)]
struct DagWorkspace {
    slots: Vec<Vec<f64>>,
    output_buf: Vec<f64>,
}

impl DagWorkspace {
    fn from_plan(plan: &DagMemoryPlan, output_numel: usize) -> Self {
        let slots = plan
            .slots
            .iter()
            .map(|s| Vec::with_capacity(s.capacity_elems))
            .collect();
        Self {
            slots,
            output_buf: Vec::with_capacity(output_numel),
        }
    }
}

/// Compiled execution plan containing a fused DAG, static buffer reuse plan, pre-decoded constants,
/// and a thread-safe checkoutable reusable [`DagWorkspace`].
#[derive(Debug)]
pub struct CompiledDagPlan {
    pub fingerprint: u64,
    pub dag: TensorComputeDag,
    pub memory_plan: DagMemoryPlan,
    const_flats: Vec<Option<Vec<f64>>>,
    output_numel: usize,
    workspace: Mutex<Option<DagWorkspace>>,
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
        }
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
    compiled_plan: Option<Arc<CompiledDagPlan>>,
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

    /// Apply all 4 operator-fusion passes (`FusedFlashAttention`, `FusedRmsNormLinear`,
    /// `FusedLinearAct`, `FusedElementwise`) and dead-node elimination.
    pub fn fuse(&self) -> Self {
        let mut nodes = self.nodes.clone();
        let mut out_id = self.output;

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

            for idx in 0..nodes.len() {
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
            compiled_plan: None,
        }
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
        execute_dag_with_workspace(self, &mem, &const_flats, &mut ws, inputs)
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

fn execute_dag_with_workspace(
    dag: &TensorComputeDag,
    mem: &DagMemoryPlan,
    const_flats: &[Option<Vec<f64>>],
    ws: &mut DagWorkspace,
    inputs: &[Arc<Tensor>],
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

    let input_flats: Vec<Vec<f64>> = inputs.iter().map(|t| t.to_flat_vec()).collect();

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
    }

    Err(MoraError::Runtime(
        "ComputeDag.run: missing output tensor".to_string(),
    ))
}

#[inline]
fn read_node_slice<'a>(
    dag: &'a TensorComputeDag,
    node_id: usize,
    input_flats: &'a [Vec<f64>],
    const_flats: &'a [Option<Vec<f64>>],
    mem: &DagMemoryPlan,
    slots: &'a [Vec<f64>],
) -> &'a [f64] {
    match &dag.nodes[node_id].op {
        DagOp::Input(slot) => &input_flats[*slot],
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
    input_flats: &[Vec<f64>],
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
