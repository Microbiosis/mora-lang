//! Wave 4: Native `TensorComputeDag`, Operator Fusion, and `DeviceBackend` Abstraction.
//!
//! Architecture (`AGENTS.md §0`):
//! - Explicit inspectable compute-graph IR (`DagNode` / `DagOp`) captured from arbitrary Mora
//!   functions via `trace_dag(f, example_inputs...)`.
//! - Real operator-fusion passes (`TensorComputeDag::fuse`):
//!   1. `FusedLinearAct`: fuses `MatMul -> (optional Add bias) -> (optional Activation)` into one
//!      single-pass kernel with zero intermediate tensor heap allocations.
//!   2. `FusedElementwise`: fuses maximal elementwise unary/binary subgraphs into one cache-local
//!      loop per element.
//!   3. Dead-node elimination (DCE) and topological index compaction.
//! - `DeviceBackend` trait (`CpuDeviceBackend` + extensible device-dispatched backend registry).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;

use crate::common::{DType, Device};
use crate::error::{MoraError, MoraResult};
use crate::runtime::autograd::{
    AutogradTape, TapeOp, matmul_f64, read_broadcast_flat, transpose_flat,
};
use crate::value::tensor::{Tensor, checked_numel, quantize_for_dtype};

/// Activation fused into `DagOp::FusedLinearAct`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    fn is_available(&self) -> bool;

    #[allow(clippy::too_many_arguments)]
    fn run_fused_linear_act(
        &self,
        x: &Tensor,
        w: &Tensor,
        bias: Option<&Tensor>,
        act: FusedAct,
        out_shape: &[usize],
        dtype: DType,
        device: Device,
    ) -> MoraResult<Arc<Tensor>>;

    fn run_fused_elementwise(
        &self,
        inputs: &[&Tensor],
        steps: &[FusedEwStep],
        out_shape: &[usize],
        dtype: DType,
        device: Device,
    ) -> MoraResult<Arc<Tensor>>;
}

/// Native host CPU backend executing cache-local fused kernels.
#[derive(Debug, Default)]
pub struct CpuDeviceBackend;

impl DeviceBackend for CpuDeviceBackend {
    fn kind(&self) -> Device {
        Device::Cpu
    }

    fn name(&self) -> &str {
        "cpu"
    }

    fn is_available(&self) -> bool {
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn run_fused_linear_act(
        &self,
        x: &Tensor,
        w: &Tensor,
        bias: Option<&Tensor>,
        act: FusedAct,
        out_shape: &[usize],
        dtype: DType,
        device: Device,
    ) -> MoraResult<Arc<Tensor>> {
        let m = x.shape[0];
        let k = x.shape[1];
        let n = w.shape[1];
        let xv = x.to_flat_vec();
        let wv = w.to_flat_vec();
        let bv = bias.map(|b| b.to_flat_vec());
        let b_shape = bias.map(|b| b.shape.as_slice());

        let mut out = vec![0.0; m * n];
        for i in 0..m {
            for p in 0..k {
                let av = xv[i * k + p];
                for j in 0..n {
                    out[i * n + j] += av * wv[p * n + j];
                }
            }
            for j in 0..n {
                let idx = i * n + j;
                let mut acc = out[idx];
                if let (Some(bvals), Some(bs)) = (&bv, b_shape) {
                    acc += read_broadcast_flat(bvals, bs, out_shape, idx);
                }
                out[idx] = quantize_for_dtype(act.eval(acc), dtype);
            }
        }
        Tensor::from_raw(out, out_shape.to_vec(), dtype, device, false)
    }

    fn run_fused_elementwise(
        &self,
        inputs: &[&Tensor],
        steps: &[FusedEwStep],
        out_shape: &[usize],
        dtype: DType,
        device: Device,
    ) -> MoraResult<Arc<Tensor>> {
        let out_numel = checked_numel(out_shape)?;
        let input_flats: Vec<Vec<f64>> = inputs.iter().map(|t| t.to_flat_vec()).collect();
        let input_shapes: Vec<&[usize]> = inputs.iter().map(|t| t.shape.as_slice()).collect();
        let n_in = inputs.len();

        let mut out = vec![0.0; out_numel];
        let mut reg_buf = vec![0.0; n_in + steps.len()];

        for (elem_idx, slot) in out.iter_mut().enumerate() {
            for i in 0..n_in {
                reg_buf[i] =
                    read_broadcast_flat(&input_flats[i], input_shapes[i], out_shape, elem_idx);
            }
            for (s_idx, step) in steps.iter().enumerate() {
                let val = match *step {
                    FusedEwStep::Add(a, b) => reg_buf[a] + reg_buf[b],
                    FusedEwStep::Sub(a, b) => reg_buf[a] - reg_buf[b],
                    FusedEwStep::Mul(a, b) => reg_buf[a] * reg_buf[b],
                    FusedEwStep::Div(a, b) => {
                        if reg_buf[b] == 0.0 {
                            return Err(MoraError::Runtime(
                                "ComputeDag fused division by zero".to_string(),
                            ));
                        }
                        reg_buf[a] / reg_buf[b]
                    }
                    FusedEwStep::Pow(a, b) => reg_buf[a].powf(reg_buf[b]),
                    FusedEwStep::AddScalar(a, c) => reg_buf[a] + c,
                    FusedEwStep::SubScalar(a, c, rev) => {
                        if rev {
                            c - reg_buf[a]
                        } else {
                            reg_buf[a] - c
                        }
                    }
                    FusedEwStep::MulScalar(a, c) => reg_buf[a] * c,
                    FusedEwStep::DivScalar(a, c, rev) => {
                        if rev {
                            if reg_buf[a] == 0.0 {
                                return Err(MoraError::Runtime(
                                    "ComputeDag fused division by zero".to_string(),
                                ));
                            }
                            c / reg_buf[a]
                        } else {
                            if c == 0.0 {
                                return Err(MoraError::Runtime(
                                    "ComputeDag fused division by zero".to_string(),
                                ));
                            }
                            reg_buf[a] / c
                        }
                    }
                    FusedEwStep::PowScalar(a, c, rev) => {
                        if rev {
                            c.powf(reg_buf[a])
                        } else {
                            reg_buf[a].powf(c)
                        }
                    }
                    FusedEwStep::Neg(a) => -reg_buf[a],
                    FusedEwStep::Relu(a) => FusedAct::Relu.eval(reg_buf[a]),
                    FusedEwStep::Gelu(a) => FusedAct::Gelu.eval(reg_buf[a]),
                    FusedEwStep::Silu(a) => FusedAct::Silu.eval(reg_buf[a]),
                    FusedEwStep::Tanh(a) => FusedAct::Tanh.eval(reg_buf[a]),
                    FusedEwStep::Sigmoid(a) => FusedAct::Sigmoid.eval(reg_buf[a]),
                    FusedEwStep::Exp(a) => reg_buf[a].exp(),
                    FusedEwStep::Ln(a) => reg_buf[a].ln(),
                    FusedEwStep::Sqrt(a) => reg_buf[a].sqrt(),
                    FusedEwStep::Sin(a) => reg_buf[a].sin(),
                    FusedEwStep::Cos(a) => reg_buf[a].cos(),
                    FusedEwStep::Abs(a) => reg_buf[a].abs(),
                };
                reg_buf[n_in + s_idx] = val;
            }
            *slot = quantize_for_dtype(reg_buf[n_in + steps.len() - 1], dtype);
        }

        Tensor::from_raw(out, out_shape.to_vec(), dtype, device, false)
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
        }
    }

    fn is_available(&self) -> bool {
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn run_fused_linear_act(
        &self,
        x: &Tensor,
        w: &Tensor,
        bias: Option<&Tensor>,
        act: FusedAct,
        out_shape: &[usize],
        dtype: DType,
        device: Device,
    ) -> MoraResult<Arc<Tensor>> {
        self.cpu
            .run_fused_linear_act(x, w, bias, act, out_shape, dtype, device)
    }

    fn run_fused_elementwise(
        &self,
        inputs: &[&Tensor],
        steps: &[FusedEwStep],
        out_shape: &[usize],
        dtype: DType,
        device: Device,
    ) -> MoraResult<Arc<Tensor>> {
        self.cpu
            .run_fused_elementwise(inputs, steps, out_shape, dtype, device)
    }
}

/// Resolve the [`DeviceBackend`] responsible for executing kernels on `device`.
pub fn backend_for_device(device: &Device) -> Arc<dyn DeviceBackend> {
    match device {
        Device::Cpu => Arc::new(CpuDeviceBackend),
        other => Arc::new(EmulatedDeviceBackend {
            device: other.clone(),
            cpu: CpuDeviceBackend,
        }),
    }
}

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
    Reshape(usize, Vec<usize>),
    Transpose(usize, usize, usize),
    Identity(usize),
    Gather(usize, Vec<usize>),
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
            Self::Reshape(_, _) => "Reshape",
            Self::Transpose(_, _, _) => "Transpose",
            Self::Identity(_) => "Identity",
            Self::Gather(_, _) => "Gather",
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
            Self::FusedElementwise { .. } => "FusedElementwise",
        }
    }

    pub fn is_fused(&self) -> bool {
        matches!(
            self,
            Self::FusedLinearAct { .. } | Self::FusedElementwise { .. }
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
            | Self::Reshape(a, _)
            | Self::Transpose(a, _, _)
            | Self::Identity(a)
            | Self::Gather(a, _)
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
            | Self::Reshape(a, _)
            | Self::Transpose(a, _, _)
            | Self::Identity(a)
            | Self::Gather(a, _)
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

/// Static inspectable compute graph captured from Mora tensor operations.
#[derive(Debug, Clone, PartialEq)]
pub struct TensorComputeDag {
    pub input_specs: Vec<(Vec<usize>, DType, Device)>,
    pub nodes: Vec<DagNode>,
    pub output: usize,
    pub fused: bool,
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
        })
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

    /// Generate a Graphviz DOT representation of this compute graph.
    pub fn to_dot(&self) -> String {
        let mut out = String::from("digraph TensorComputeDag {\n  rankdir=TB;\n");
        for (idx, n) in self.nodes.iter().enumerate() {
            let style = if n.op.is_fused() {
                ", style=filled, fillcolor=\"#dbeafe\""
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "  n{idx} [label=\"{}: {:?} {}\"{}];",
                n.op.name(),
                n.shape,
                n.dtype.short_name(),
                style
            );
            for inp in n.op.inputs() {
                let _ = writeln!(out, "  n{inp} -> n{idx};");
            }
        }
        let _ = writeln!(out, "}}");
        out
    }

    /// Apply operator fusion (`FusedLinearAct` + `FusedElementwise`) and dead-node elimination.
    pub fn fuse(&self) -> Self {
        let mut nodes = self.nodes.clone();
        let mut out_id = self.output;

        // Compute consumer counts.
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

        // Pass 1: Fuse `Act(Add(MatMul(x, w), bias))` / `Add(MatMul(x, w), bias)` / `Act(MatMul(x, w))`
        // for 2D MatMul `(M, K) @ (K, N)`.
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
        let is_2d_matmul = |ns: &[DagNode], mm_id: usize| -> Option<(usize, usize)> {
            if let DagOp::MatMul(x, w) = ns[mm_id].op
                && ns[x].shape.len() == 2
                && ns[w].shape.len() == 2
                && ns[mm_id].shape.len() == 2
            {
                Some((x, w))
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
                        && let Some((x, w)) = is_2d_matmul(&nodes, lhs)
                        && nodes[inner].shape == nodes[lhs].shape
                    {
                        nodes[idx].op = DagOp::FusedLinearAct {
                            x,
                            w,
                            bias: Some(rhs),
                            act,
                        };
                        continue;
                    }
                    if uses[rhs] == 1
                        && let Some((x, w)) = is_2d_matmul(&nodes, rhs)
                        && nodes[inner].shape == nodes[rhs].shape
                    {
                        nodes[idx].op = DagOp::FusedLinearAct {
                            x,
                            w,
                            bias: Some(lhs),
                            act,
                        };
                        continue;
                    }
                }
                if let Some((x, w)) = is_2d_matmul(&nodes, inner) {
                    nodes[idx].op = DagOp::FusedLinearAct {
                        x,
                        w,
                        bias: None,
                        act,
                    };
                    continue;
                }
            }
            if let DagOp::Add(lhs, rhs) = nodes[idx].op {
                if uses[lhs] == 1
                    && let Some((x, w)) = is_2d_matmul(&nodes, lhs)
                    && nodes[idx].shape == nodes[lhs].shape
                {
                    nodes[idx].op = DagOp::FusedLinearAct {
                        x,
                        w,
                        bias: Some(rhs),
                        act: FusedAct::None,
                    };
                    continue;
                }
                if uses[rhs] == 1
                    && let Some((x, w)) = is_2d_matmul(&nodes, rhs)
                    && nodes[idx].shape == nodes[rhs].shape
                {
                    nodes[idx].op = DagOp::FusedLinearAct {
                        x,
                        w,
                        bias: Some(lhs),
                        act: FusedAct::None,
                    };
                }
            }
        }

        // Compact dead nodes before elementwise chain fusion.
        let (mut nodes, compacted_out) = compact_live_nodes(nodes, out_id);
        out_id = compacted_out;

        // Pass 2: Fuse maximal single-consumer chains of elementwise operations.
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
            // Collect a connected DAG of elementwise producers exclusively feeding `idx`.
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
        }
    }

    /// Execute this compute graph on `inputs` via [`backend_for_device`].
    pub fn run(&self, inputs: &[Arc<Tensor>]) -> MoraResult<Arc<Tensor>> {
        if inputs.len() != self.input_specs.len() {
            return Err(MoraError::Runtime(format!(
                "ComputeDag.run: expected {} input tensor(s), got {}",
                self.input_specs.len(),
                inputs.len()
            )));
        }
        for (i, (inp, (exp_shape, exp_dt, exp_dev))) in
            inputs.iter().zip(self.input_specs.iter()).enumerate()
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

        let mut values: Vec<Option<Arc<Tensor>>> = vec![None; self.nodes.len()];
        for (idx, node) in self.nodes.iter().enumerate() {
            let backend = backend_for_device(&node.device);
            let out_t = match &node.op {
                DagOp::Input(slot) => Arc::clone(&inputs[*slot]),
                DagOp::ConstTensor(t) => Arc::clone(t),
                DagOp::FusedLinearAct { x, w, bias, act } => {
                    let xt = values[*x].as_ref().expect("x ready");
                    let wt = values[*w].as_ref().expect("w ready");
                    let bt = bias.map(|b| values[b].as_ref().expect("bias ready").as_ref());
                    backend.run_fused_linear_act(
                        xt,
                        wt,
                        bt,
                        *act,
                        &node.shape,
                        node.dtype,
                        node.device.clone(),
                    )?
                }
                DagOp::FusedElementwise { inputs: ids, steps } => {
                    let refs: Vec<&Tensor> = ids
                        .iter()
                        .map(|&id| values[id].as_ref().expect("ew input ready").as_ref())
                        .collect();
                    backend.run_fused_elementwise(
                        &refs,
                        steps,
                        &node.shape,
                        node.dtype,
                        node.device.clone(),
                    )?
                }
                DagOp::Add(a, b) => {
                    let at = values[*a].as_ref().expect("a ready").as_ref();
                    let bt = values[*b].as_ref().expect("b ready").as_ref();
                    backend.run_fused_elementwise(
                        &[at, bt],
                        &[FusedEwStep::Add(0, 1)],
                        &node.shape,
                        node.dtype,
                        node.device.clone(),
                    )?
                }
                DagOp::Sub(a, b) => {
                    let at = values[*a].as_ref().expect("a ready").as_ref();
                    let bt = values[*b].as_ref().expect("b ready").as_ref();
                    backend.run_fused_elementwise(
                        &[at, bt],
                        &[FusedEwStep::Sub(0, 1)],
                        &node.shape,
                        node.dtype,
                        node.device.clone(),
                    )?
                }
                DagOp::Mul(a, b) => {
                    let at = values[*a].as_ref().expect("a ready").as_ref();
                    let bt = values[*b].as_ref().expect("b ready").as_ref();
                    backend.run_fused_elementwise(
                        &[at, bt],
                        &[FusedEwStep::Mul(0, 1)],
                        &node.shape,
                        node.dtype,
                        node.device.clone(),
                    )?
                }
                DagOp::Div(a, b) => {
                    let at = values[*a].as_ref().expect("a ready").as_ref();
                    let bt = values[*b].as_ref().expect("b ready").as_ref();
                    backend.run_fused_elementwise(
                        &[at, bt],
                        &[FusedEwStep::Div(0, 1)],
                        &node.shape,
                        node.dtype,
                        node.device.clone(),
                    )?
                }
                DagOp::Pow(a, b) => {
                    let at = values[*a].as_ref().expect("a ready").as_ref();
                    let bt = values[*b].as_ref().expect("b ready").as_ref();
                    backend.run_fused_elementwise(
                        &[at, bt],
                        &[FusedEwStep::Pow(0, 1)],
                        &node.shape,
                        node.dtype,
                        node.device.clone(),
                    )?
                }
                DagOp::AddScalar(a, c) => {
                    let at = values[*a].as_ref().expect("a ready").as_ref();
                    backend.run_fused_elementwise(
                        &[at],
                        &[FusedEwStep::AddScalar(0, *c)],
                        &node.shape,
                        node.dtype,
                        node.device.clone(),
                    )?
                }
                DagOp::SubScalar(a, c, r) => {
                    let at = values[*a].as_ref().expect("a ready").as_ref();
                    backend.run_fused_elementwise(
                        &[at],
                        &[FusedEwStep::SubScalar(0, *c, *r)],
                        &node.shape,
                        node.dtype,
                        node.device.clone(),
                    )?
                }
                DagOp::MulScalar(a, c) => {
                    let at = values[*a].as_ref().expect("a ready").as_ref();
                    backend.run_fused_elementwise(
                        &[at],
                        &[FusedEwStep::MulScalar(0, *c)],
                        &node.shape,
                        node.dtype,
                        node.device.clone(),
                    )?
                }
                DagOp::DivScalar(a, c, r) => {
                    let at = values[*a].as_ref().expect("a ready").as_ref();
                    backend.run_fused_elementwise(
                        &[at],
                        &[FusedEwStep::DivScalar(0, *c, *r)],
                        &node.shape,
                        node.dtype,
                        node.device.clone(),
                    )?
                }
                DagOp::PowScalar(a, c, r) => {
                    let at = values[*a].as_ref().expect("a ready").as_ref();
                    backend.run_fused_elementwise(
                        &[at],
                        &[FusedEwStep::PowScalar(0, *c, *r)],
                        &node.shape,
                        node.dtype,
                        node.device.clone(),
                    )?
                }
                DagOp::Neg(a) => run_single_ew(
                    backend.as_ref(),
                    values[*a].as_ref().expect("a"),
                    FusedEwStep::Neg(0),
                    node,
                )?,
                DagOp::Relu(a) => run_single_ew(
                    backend.as_ref(),
                    values[*a].as_ref().expect("a"),
                    FusedEwStep::Relu(0),
                    node,
                )?,
                DagOp::Gelu(a) => run_single_ew(
                    backend.as_ref(),
                    values[*a].as_ref().expect("a"),
                    FusedEwStep::Gelu(0),
                    node,
                )?,
                DagOp::Silu(a) => run_single_ew(
                    backend.as_ref(),
                    values[*a].as_ref().expect("a"),
                    FusedEwStep::Silu(0),
                    node,
                )?,
                DagOp::Tanh(a) => run_single_ew(
                    backend.as_ref(),
                    values[*a].as_ref().expect("a"),
                    FusedEwStep::Tanh(0),
                    node,
                )?,
                DagOp::Sigmoid(a) => run_single_ew(
                    backend.as_ref(),
                    values[*a].as_ref().expect("a"),
                    FusedEwStep::Sigmoid(0),
                    node,
                )?,
                DagOp::Exp(a) => run_single_ew(
                    backend.as_ref(),
                    values[*a].as_ref().expect("a"),
                    FusedEwStep::Exp(0),
                    node,
                )?,
                DagOp::Ln(a) => run_single_ew(
                    backend.as_ref(),
                    values[*a].as_ref().expect("a"),
                    FusedEwStep::Ln(0),
                    node,
                )?,
                DagOp::Sqrt(a) => run_single_ew(
                    backend.as_ref(),
                    values[*a].as_ref().expect("a"),
                    FusedEwStep::Sqrt(0),
                    node,
                )?,
                DagOp::Sin(a) => run_single_ew(
                    backend.as_ref(),
                    values[*a].as_ref().expect("a"),
                    FusedEwStep::Sin(0),
                    node,
                )?,
                DagOp::Cos(a) => run_single_ew(
                    backend.as_ref(),
                    values[*a].as_ref().expect("a"),
                    FusedEwStep::Cos(0),
                    node,
                )?,
                DagOp::Abs(a) => run_single_ew(
                    backend.as_ref(),
                    values[*a].as_ref().expect("a"),
                    FusedEwStep::Abs(0),
                    node,
                )?,
                DagOp::MatMul(a, b) => {
                    let at = values[*a].as_ref().expect("a ready");
                    let bt = values[*b].as_ref().expect("b ready");
                    let out =
                        matmul_f64(&at.to_flat_vec(), &bt.to_flat_vec(), &at.shape, &bt.shape)?;
                    Tensor::from_raw(
                        out,
                        node.shape.clone(),
                        node.dtype,
                        node.device.clone(),
                        false,
                    )?
                }
                DagOp::Sum(a) => {
                    let at = values[*a].as_ref().expect("a ready");
                    let s: f64 = at.to_flat_vec().into_iter().sum();
                    Tensor::from_raw(
                        vec![s],
                        node.shape.clone(),
                        node.dtype,
                        node.device.clone(),
                        false,
                    )?
                }
                DagOp::Mean(a) => {
                    let at = values[*a].as_ref().expect("a ready");
                    let flat = at.to_flat_vec();
                    let m = if flat.is_empty() {
                        0.0
                    } else {
                        flat.iter().sum::<f64>() / (flat.len() as f64)
                    };
                    Tensor::from_raw(
                        vec![m],
                        node.shape.clone(),
                        node.dtype,
                        node.device.clone(),
                        false,
                    )?
                }
                DagOp::Reshape(a, target_shape) => values[*a]
                    .as_ref()
                    .expect("a ready")
                    .reshape(target_shape.clone())?,
                DagOp::Transpose(a, d0, d1) => {
                    let at = values[*a].as_ref().expect("a ready");
                    let flat = at.to_flat_vec();
                    let tr = transpose_flat(&flat, &at.shape, *d0, *d1, &node.shape);
                    Tensor::from_raw(
                        tr,
                        node.shape.clone(),
                        node.dtype,
                        node.device.clone(),
                        false,
                    )?
                }
                DagOp::Identity(a) => Arc::clone(values[*a].as_ref().expect("a ready")),
                DagOp::Gather(a, indices) => {
                    let at = values[*a].as_ref().expect("a ready");
                    let flat = at.to_flat_vec();
                    let gathered: Vec<f64> = indices.iter().map(|&i| flat[i]).collect();
                    Tensor::from_raw(
                        gathered,
                        node.shape.clone(),
                        node.dtype,
                        node.device.clone(),
                        false,
                    )?
                }
                DagOp::Softmax(a) => values[*a].as_ref().expect("a ready").softmax()?,
                DagOp::RmsNorm(a, eps) => values[*a].as_ref().expect("a ready").rms_norm(*eps)?,
                DagOp::LayerNorm(a, eps) => {
                    values[*a].as_ref().expect("a ready").layer_norm(*eps)?
                }
            };
            values[idx] = Some(out_t);
        }

        values[self.output]
            .clone()
            .ok_or_else(|| MoraError::Runtime("ComputeDag.run: missing output tensor".to_string()))
    }
}

fn run_single_ew(
    backend: &dyn DeviceBackend,
    t: &Arc<Tensor>,
    step: FusedEwStep,
    node: &DagNode,
) -> MoraResult<Arc<Tensor>> {
    backend.run_fused_elementwise(
        &[t.as_ref()],
        &[step],
        &node.shape,
        node.dtype,
        node.device.clone(),
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
