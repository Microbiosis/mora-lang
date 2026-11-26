use super::*;

/// 最后一维上每行独立归一化。训练和推理共用数值定义。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowNorm {
    Rms,
    Layer,
}

/// Calculations use (x - origin) / magnitude, including epsilon in those same units.
/// Translating before computing the mean retains adjacent floats at a large common offset.
/// Scaling before squaring avoids overflow and underflow in the sum of squares.
struct RowMoments {
    origin: f64,
    magnitude: f64,
    mean: f64,
    deviation: f64,
}

impl RowMoments {
    fn normalized(&self, x: f64) -> f64 {
        ((x - self.origin) / self.magnitude - self.mean) / self.deviation
    }
}

impl RowNorm {
    fn name(self) -> &'static str {
        match self {
            Self::Rms => "rms_norm",
            Self::Layer => "layer_norm",
        }
    }

    fn moments(self, width: usize, eps: f64, read: impl Fn(usize) -> f64) -> RowMoments {
        let (mut low, mut high) = (0.0_f64, 0.0_f64);
        if self == Self::Layer {
            low = read(0);
            high = low;
        }
        for i in 0..width {
            let x = read(i);
            low = low.min(x);
            high = high.max(x);
        }
        // The endpoint nearest zero keeps the difference representable for either sign.
        let origin = low.max(0.0) + high.min(0.0);
        let root_eps = eps.abs().sqrt().copysign(eps);
        let mut magnitude = (high - origin)
            .abs()
            .max((low - origin).abs())
            .max(root_eps.abs());
        if magnitude == 0.0 || !magnitude.is_finite() {
            magnitude = 1.0;
        }
        let scaled = |i| (read(i) - origin) / magnitude;
        let mean = if self == Self::Layer {
            (0..width).map(scaled).sum::<f64>() / width as f64
        } else {
            0.0
        };
        let squares = (0..width).map(|i| (scaled(i) - mean).powi(2)).sum::<f64>();
        let scaled_eps = root_eps / magnitude;
        let deviation = (squares / width as f64 + scaled_eps * scaled_eps.abs()).sqrt();
        RowMoments {
            origin,
            magnitude,
            mean,
            deviation,
        }
    }

    pub(crate) fn write_row(self, input: &[f64], eps: f64, dtype: DType, output: &mut [f64]) {
        let moments = self.moments(input.len(), eps, |i| input[i]);
        for (&x, y) in input.iter().zip(output) {
            *y = quantize_for_dtype(moments.normalized(x), dtype);
        }
    }

    /// The row Jacobian is symmetric, so forward and reverse AD share this product.
    pub(crate) fn differential(
        self,
        input: &[f64],
        vector: &[f64],
        width: usize,
        eps: f64,
    ) -> Vec<f64> {
        let mut result = Vec::with_capacity(input.len());
        for (row, v) in input.chunks_exact(width).zip(vector.chunks_exact(width)) {
            let moments = self.moments(width, eps, |i| row[i]);
            let mean = if self == Self::Layer {
                v.iter().sum::<f64>() / width as f64
            } else {
                0.0
            };
            let dot = row
                .iter()
                .zip(v)
                .map(|(&x, &d)| moments.normalized(x) * d)
                .sum::<f64>()
                / width as f64;
            result.extend(row.iter().zip(v).map(|(&x, &d)| {
                ((d - mean - moments.normalized(x) * dot) / moments.deviation) / moments.magnitude
            }));
        }
        result
    }

    fn check_dtype(self, t: &Tensor) -> MoraResult<()> {
        if t.dtype.is_float() {
            Ok(())
        } else {
            Err(MoraError::Runtime(format!(
                "Tensor.{} requires a floating-point dtype, got {}",
                self.name(),
                t.dtype
            )))
        }
    }

    pub(super) fn allocated(self, t: &Tensor, eps: f64) -> MoraResult<Arc<Tensor>> {
        self.check_dtype(t)?;
        if let Some(input) = t.vram_buffer_if_contiguous() {
            let backend = crate::runtime::compute_dag::backend_for_device(&t.device);
            if let Some(output) = backend.alloc_vram_buffer(t.nbytes())?
                && self.launch(t, input.as_ref(), output.as_ref(), eps)?
            {
                return Tensor::from_vram_output(
                    output,
                    t.shape.clone(),
                    t.dtype,
                    t.device.clone(),
                    t.requires_grad,
                );
            }
        }
        let mut flat = t.to_flat_vec();
        let width = t.shape.last().copied().unwrap_or(1);
        for row in flat.chunks_mut(width) {
            let moments = self.moments(width, eps, |i| row[i]);
            for x in row {
                *x = moments.normalized(*x);
            }
        }
        Tensor::from_raw(
            flat,
            t.shape.clone(),
            t.dtype,
            t.device.clone(),
            t.requires_grad,
        )
    }

    pub(super) fn reuse(
        self,
        t: &Arc<Tensor>,
        eps: f64,
        stats: Option<&crate::runtime::fbip::FbipCounters>,
    ) -> MoraResult<()> {
        self.check_dtype(t)?;
        reuse_storage(
            t,
            stats,
            |buf| self.launch(t, buf.as_ref(), buf.as_ref(), eps),
            |data| {
                self.update_host(t, eps, data);
                Ok(())
            },
        )
    }

    fn update_host(self, t: &Tensor, eps: f64, data: &mut AlignedByteBuffer) {
        let width = t.shape.last().copied().unwrap_or(1);
        for base in (0..data.len()).step_by(width) {
            let moments = self.moments(width, eps, |i| data.get_f64(base + i));
            for i in base..base + width {
                let x = data.get_f64(i);
                data.set_f64(i, moments.normalized(x));
            }
        }
    }

    fn launch(
        self,
        t: &Tensor,
        input: &dyn DeviceVramBuffer,
        output: &dyn DeviceVramBuffer,
        eps: f64,
    ) -> MoraResult<bool> {
        use crate::runtime::gpu::kernel_compiler::KernelLaunchArg;
        let Some(kernel) = kernel::for_type(self, t.dtype) else {
            return Ok(false);
        };
        // 内核的行起止下标使用 i32，留出最后一个工作组的补齐空间。
        if t.numel() > (i32::MAX as usize - 128) {
            return Ok(false);
        }
        let width = t.shape.last().copied().unwrap_or(1);
        let rows = t.numel() / width;
        let buf = |b: &dyn DeviceVramBuffer| KernelLaunchArg::VramBuffer {
            handle: b.raw_handle(),
            byte_len: b.byte_len(),
            dtype: t.dtype,
        };
        let root_eps = eps.abs().sqrt().copysign(eps);
        let epsilon = if t.dtype == DType::F64 {
            KernelLaunchArg::F64(root_eps)
        } else {
            let root = root_eps as f32;
            if root_eps.is_finite() && (!root.is_finite() || (root == 0.0 && root_eps != 0.0)) {
                return Ok(false);
            }
            KernelLaunchArg::F32(root)
        };
        let args = [
            buf(input),
            buf(output),
            KernelLaunchArg::I32(width as i32),
            KernelLaunchArg::I32(rows as i32),
            epsilon,
        ];
        let backend = crate::runtime::compute_dag::backend_for_device(&t.device);
        backend.launch_custom_kernel(kernel, [rows.div_ceil(128) as u32, 1, 1], [64, 1, 1], &args)
    }
}

mod kernel {
    use super::RowNorm;
    use crate::common::DType;
    use crate::common::kernel_ir::{
        KernelBinOp as Op, KernelExpr as Expr, KernelParam, KernelParamType as Param,
        KernelProgram, KernelScalarType as Ty, KernelStmt as Stmt, KernelUnaryOp as Unary,
    };
    use std::sync::OnceLock;

    pub(super) fn for_type(norm: RowNorm, dtype: DType) -> Option<&'static KernelProgram> {
        static PROGRAMS: [[OnceLock<KernelProgram>; 4]; 2] =
            [const { [const { OnceLock::new() }; 4] }; 2];
        let (index, buf) = match dtype {
            DType::F64 => (0, Param::BufF64),
            DType::F32 => (1, Param::BufF32),
            DType::F16 => (2, Param::BufF16),
            DType::BF16 => (3, Param::BufBF16),
            _ => return None,
        };
        Some(PROGRAMS[usize::from(norm == RowNorm::Layer)][index].get_or_init(|| build(norm, buf)))
    }

    fn bin(op: Op, left: Expr, right: Expr) -> Expr {
        let ty = if op == Op::Lt { Ty::Bool } else { left.ty() };
        Expr::Binary {
            op,
            lhs: Box::new(left),
            rhs: Box::new(right),
            ty,
        }
    }

    fn unary(op: Unary, arg: Expr, ty: Ty) -> Expr {
        Expr::Unary {
            op,
            arg: Box::new(arg),
            ty,
        }
    }

    fn var(name: &str, ty: Ty) -> Expr {
        Expr::Var(name.into(), ty)
    }
    fn declare(name: &str, expr: Expr) -> Stmt {
        Stmt::Let {
            name: name.into(),
            ty: expr.ty(),
            expr,
        }
    }
    fn set(name: &str, expr: Expr) -> Stmt {
        Stmt::Assign {
            name: name.into(),
            expr,
        }
    }

    fn scan(mut action: Vec<Stmt>) -> Stmt {
        let index = var("i", Ty::I32);
        action.push(set("i", bin(Op::Add, index.clone(), Expr::ConstI32(1))));
        Stmt::While {
            cond: bin(Op::Lt, index, var("end_index", Ty::I32)),
            body: action,
        }
    }

    fn build(norm: RowNorm, buf: Param) -> KernelProgram {
        let double = buf == Param::BufF64;
        let ty = if double { Ty::F64 } else { Ty::F32 };
        let zero = if double {
            Expr::ConstF64(0.)
        } else {
            Expr::ConstF32(0.)
        };
        let one = if double {
            Expr::ConstF64(1.)
        } else {
            Expr::ConstF32(1.)
        };
        let width = Expr::Param(2, Ty::I32);
        let rows = Expr::Param(3, Ty::I32);
        let width_float = unary(
            if double { Unary::ToF64 } else { Unary::ToF32 },
            width.clone(),
            ty,
        );
        let row = var("row", Ty::I32);
        let start = var("start_index", Ty::I32);
        let load = |index| Expr::Load {
            buf_param: 0,
            index: Box::new(index),
            elem_ty: ty,
        };
        let value = load(var("i", Ty::I32));
        let first = if norm == RowNorm::Layer {
            load(start.clone())
        } else {
            zero.clone()
        };
        let low = var("low", ty);
        let high = var("high", ty);
        let origin = var("origin", ty);
        let magnitude = var("magnitude", ty);
        let mean = var("mean", ty);
        let squares = var("squares", ty);
        let root_eps = Expr::Param(4, ty);
        let abs = |v| unary(Unary::Abs, v, ty);
        let scaled = bin(
            Op::Div,
            bin(Op::Sub, value.clone(), origin.clone()),
            magnitude.clone(),
        );
        let delta = bin(Op::Sub, scaled.clone(), mean.clone());
        let max_finite = if double {
            Expr::ConstF64(f64::MAX)
        } else {
            Expr::ConstF32(f32::MAX)
        };
        let mut row_body = vec![
            declare("start_index", bin(Op::Mul, row.clone(), width.clone())),
            declare("end_index", bin(Op::Add, start.clone(), width)),
            declare("i", start.clone()),
            declare("low", first.clone()),
            declare("high", first),
            scan(vec![
                set("low", bin(Op::Min, low.clone(), value.clone())),
                set("high", bin(Op::Max, high.clone(), value)),
            ]),
            declare(
                "origin",
                bin(
                    Op::Add,
                    bin(Op::Max, low.clone(), zero.clone()),
                    bin(Op::Min, high.clone(), zero.clone()),
                ),
            ),
            declare(
                "magnitude",
                bin(
                    Op::Max,
                    abs(root_eps.clone()),
                    bin(
                        Op::Max,
                        abs(bin(Op::Sub, low, origin.clone())),
                        abs(bin(Op::Sub, high, origin)),
                    ),
                ),
            ),
            Stmt::If {
                cond: bin(Op::Lt, zero.clone(), magnitude.clone()),
                then_body: vec![Stmt::If {
                    cond: bin(Op::Lt, max_finite, magnitude.clone()),
                    then_body: vec![set("magnitude", one.clone())],
                    else_body: Vec::new(),
                }],
                else_body: vec![set("magnitude", one)],
            },
            declare("mean", zero.clone()),
            set("i", start.clone()),
        ];
        if norm == RowNorm::Layer {
            row_body.push(scan(vec![set("mean", bin(Op::Add, mean.clone(), scaled))]));
            row_body.push(set("mean", bin(Op::Div, mean, width_float.clone())));
            row_body.push(set("i", start.clone()));
        }
        row_body.push(declare("squares", zero));
        row_body.push(scan(vec![set(
            "squares",
            bin(
                Op::Add,
                squares.clone(),
                bin(Op::Mul, delta.clone(), delta.clone()),
            ),
        )]));
        let scaled_eps = bin(Op::Div, root_eps, magnitude);
        row_body.push(declare(
            "deviation",
            unary(
                Unary::Sqrt,
                bin(
                    Op::Add,
                    bin(Op::Div, squares, width_float),
                    bin(Op::Mul, scaled_eps.clone(), abs(scaled_eps)),
                ),
                ty,
            ),
        ));
        row_body.push(set("i", start));
        row_body.push(scan(vec![Stmt::Store {
            buf_param: 1,
            index: var("i", Ty::I32),
            value: bin(Op::Div, delta, var("deviation", ty)),
        }]));
        row_body.push(set("row", bin(Op::Add, row.clone(), Expr::ConstI32(1))));
        // 相邻两行交给同一个线程，奇数行宽的半精度数据也不会跨线程改写同一物理字。
        let body = vec![
            declare("row", bin(Op::Mul, Expr::GlobalId, Expr::ConstI32(2))),
            declare(
                "end_row",
                bin(Op::Min, bin(Op::Add, row.clone(), Expr::ConstI32(2)), rows),
            ),
            Stmt::While {
                cond: bin(Op::Lt, row, var("end_row", Ty::I32)),
                body: row_body,
            },
        ];
        let params = [
            ("input", buf),
            ("output", buf),
            ("width", Param::I32),
            ("rows", Param::I32),
            ("root_epsilon", if double { Param::F64 } else { Param::F32 }),
        ]
        .into_iter()
        .map(|(name, ty)| KernelParam {
            name: name.into(),
            ty,
        })
        .collect();
        KernelProgram {
            name: format!(
                "mora_{}_{}",
                norm.name(),
                buf.buffer_dtype().expect("浮点缓冲区").short_name()
            ),
            params,
            shared: Vec::new(),
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::{fbip::FbipCounters, gpu::list_hardware_devices};

    fn tensor_result(result: Value) -> Arc<Tensor> {
        let Value::Tensor(t) = result else {
            panic!("应返回张量")
        };
        t
    }

    #[test]
    fn row_norm_matches_for_owned_shared_and_strided_extreme_rows() {
        for info in list_hardware_devices() {
            for dtype in [DType::F32, DType::F64] {
                let device = &info.device;
                let offset = 65536.;
                let next = offset + 0.0078125;
                let v = (next - offset) * 0.5;
                let y = v / (v * v + 1e-5_f64).sqrt();
                let large = if dtype == DType::F32 { 1e20 } else { 1e300 };
                for (norm, values, expected) in [
                    (RowNorm::Layer, [offset, next, next, offset], [-y, y, y, -y]),
                    (
                        RowNorm::Rms,
                        [large, -large, -large, large],
                        [1., -1., -1., 1.],
                    ),
                ] {
                    let input =
                        Tensor::from_raw(values.to_vec(), vec![2, 2], dtype, device.clone(), false)
                            .unwrap();
                    let view = input.transpose(None, None).unwrap();
                    let logical = norm.allocated(&view, 1e-5).unwrap().to_flat_vec();
                    let shared = norm.allocated(&input, 1e-5).unwrap().to_flat_vec();
                    drop(view);
                    let identity = input.storage_id();
                    let TensorMethodDispatch::Handled(result) =
                        call_tensor_method_fbip(input, norm.name(), &[], None, None)
                    else {
                        panic!("应分派归一化方法")
                    };
                    let owned = tensor_result(result.unwrap());
                    assert_eq!(owned.storage_id(), identity);
                    for got in [logical, shared, owned.to_flat_vec()] {
                        for (&x, y) in got.iter().zip(expected) {
                            assert!(
                                (x - y).abs() < 2e-6,
                                "{device} {dtype} {}: {x} vs {y}",
                                norm.name()
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn elementwise_reuses_the_same_allocation_on_every_available_device() {
        for info in list_hardware_devices() {
            let device = &info.device;
            let make =
                |data| Tensor::from_raw(data, vec![2], DType::F64, device.clone(), false).unwrap();
            let stats = FbipCounters::default();
            let x = make(vec![-2., 3.]);
            let identity = x.storage_id();
            let x =
                tensor_result(eval_tensor_math_unary_fbip(x, "relu", None, Some(&stats)).unwrap());
            let bin = |op, a, b| {
                tensor_result(eval_tensor_binary_fbip(&op, a, b, None, Some(&stats)).unwrap())
            };
            let x = bin(BinaryOp::Add, Value::Tensor(x), Value::Float(2.));
            let x = bin(BinaryOp::Sub, Value::Float(10.), Value::Tensor(x));
            let x = bin(
                BinaryOp::Mul,
                Value::Tensor(x),
                Value::Tensor(make(vec![3., 2.])),
            );
            let shared = make(vec![30., 20.]);
            let x = bin(
                BinaryOp::Sub,
                Value::Tensor(shared.clone()),
                Value::Tensor(x),
            );
            assert_eq!(x.storage_id(), identity, "{device}");
            assert_eq!(x.to_flat_vec(), [6., 10.]);
            assert_eq!(shared.to_flat_vec(), [30., 20.]);
            let counter = if matches!(device, Device::Cpu) {
                &stats.tensor_host_reuses
            } else {
                &stats.tensor_vram_reuses
            };
            assert_eq!(counter.load(Ordering::Relaxed), 5, "{device}");
        }
    }
}
