//! P4-1B: Source-level GPU `kernel` compiler & multi-backend code generator.
//!
//! Compiles typed Mora `WitnessKind::KernelDef` bodies into [`KernelProgram`] IR,
//! and emits native compute code for all 4 execution targets with zero external dependencies:
//! 1. **CUDA PTX ISA 7.0 (`sm_50+`)** — pure ASCII PTX for NVIDIA Driver API JIT (`cuModuleLoadDataEx`)
//! 2. **Vulkan SPIR-V 1.0 (`GLCompute` + `GLSL.std.450`)** — structured CFG binary shader words
//! 3. **Metal Shading Language (MSL)** — macOS Apple Silicon / AMD / Intel GPU compute source
//! 4. **CPU Parallel Fallback** — bounds-checked host executor for `Device::Cpu` and `MORA_FORCE_CPU_EMULATION=1`

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::sync::Arc;

use crate::common::kernel_ir::{
    KernelBinOp, KernelExpr, KernelParam, KernelParamType, KernelProgram, KernelScalarType,
    KernelStmt, KernelUnaryOp,
};
use crate::common::{BinaryOp, DType, Literal};
use crate::error::{MoraError, MoraResult};
use crate::mir::witness::{BuiltinOp, MirWitness, WitnessCallee, WitnessKind};

/// Runtime value of a compiled first-class GPU `kernel` (`Value::GpuKernel`).
#[derive(Debug, Clone)]
pub struct CompiledGpuKernel {
    pub program: KernelProgram,
    pub hash: u64,
    written_bufs: Vec<usize>,
}

impl CompiledGpuKernel {
    pub fn from_program(program: KernelProgram) -> Self {
        let hash = program.content_hash();
        let written_bufs = collect_written_buf_params(&program.body);
        Self {
            program,
            hash,
            written_bufs,
        }
    }

    pub fn name(&self) -> &str {
        &self.program.name
    }

    pub fn params(&self) -> &[KernelParam] {
        &self.program.params
    }

    pub fn written_buf_params(&self) -> &[usize] {
        &self.written_bufs
    }

    pub fn emit_ptx(&self) -> String {
        emit_ptx(&self.program)
    }

    pub fn emit_spirv_words(&self, local_size_x: u32) -> Result<Vec<u32>, String> {
        emit_spirv_words(&self.program, local_size_x)
    }

    pub fn emit_msl(&self) -> String {
        emit_msl(&self.program)
    }
}

/// Hardware launch argument passed into `DeviceBackend::launch_custom_kernel`.
#[derive(Debug, Clone)]
pub enum KernelLaunchArg {
    F32(f32),
    F64(f64),
    I32(i32),
    VramBuffer {
        handle: u64,
        byte_len: usize,
        dtype: DType,
    },
}

fn collect_written_buf_params(stmts: &[KernelStmt]) -> Vec<usize> {
    let mut set = HashSet::new();
    fn visit(stmts: &[KernelStmt], set: &mut HashSet<usize>) {
        for s in stmts {
            match s {
                KernelStmt::Store { buf_param, .. } => {
                    set.insert(*buf_param);
                }
                KernelStmt::If {
                    then_body,
                    else_body,
                    ..
                } => {
                    visit(then_body, set);
                    visit(else_body, set);
                }
                KernelStmt::While { body, .. } => visit(body, set),
                KernelStmt::Let { .. } | KernelStmt::Assign { .. } => {}
            }
        }
    }
    visit(stmts, &mut set);
    let mut out: Vec<usize> = set.into_iter().collect();
    out.sort_unstable();
    out
}

// ============================================================================
// 1. MirWitness -> Typed KernelProgram Lowering & Static Verification
// ============================================================================

struct KernelLowerCtx {
    kernel_name: String,
    scalar_params: HashMap<String, (usize, KernelScalarType)>,
    buf_params: HashMap<String, (usize, KernelScalarType)>,
    locals: HashMap<String, KernelScalarType>,
}

impl KernelLowerCtx {
    fn new(name: &str, params: &[KernelParam]) -> Self {
        let mut scalar_params = HashMap::new();
        let mut buf_params = HashMap::new();
        for (idx, p) in params.iter().enumerate() {
            match p.ty {
                KernelParamType::F32 => {
                    scalar_params.insert(p.name.clone(), (idx, KernelScalarType::F32));
                }
                KernelParamType::F64 => {
                    scalar_params.insert(p.name.clone(), (idx, KernelScalarType::F64));
                }
                KernelParamType::I32 => {
                    scalar_params.insert(p.name.clone(), (idx, KernelScalarType::I32));
                }
                KernelParamType::BufF32 => {
                    buf_params.insert(p.name.clone(), (idx, KernelScalarType::F32));
                }
                KernelParamType::BufF64 => {
                    buf_params.insert(p.name.clone(), (idx, KernelScalarType::F64));
                }
            }
        }
        Self {
            kernel_name: name.to_string(),
            scalar_params,
            buf_params,
            locals: HashMap::new(),
        }
    }

    fn lower_block(&mut self, witnesses: &[MirWitness]) -> Result<Vec<KernelStmt>, String> {
        let mut out = Vec::new();
        for w in witnesses {
            self.lower_stmt(w, &mut out)?;
        }
        Ok(out)
    }

    fn lower_stmt(&mut self, w: &MirWitness, out: &mut Vec<KernelStmt>) -> Result<(), String> {
        match &w.kind {
            WitnessKind::Sequence(items) => {
                for item in items {
                    self.lower_stmt(item, out)?;
                }
                Ok(())
            }
            WitnessKind::Literal(Literal::Nil(_)) => Ok(()),
            WitnessKind::LetBinding {
                name,
                type_hint,
                value,
                ..
            } => {
                let expected = match type_hint {
                    Some(h) => Some(hint_to_kernel_scalar(h.to_type(), &self.kernel_name, name)?),
                    None => None,
                };
                let expr = self.lower_expr(value, expected)?;
                let ty = expr.ty();
                if let Some(exp) = expected
                    && exp != ty
                {
                    return Err(format!(
                        "kernel '{}': local '{}' annotated as {} but initialized with {}",
                        self.kernel_name,
                        name,
                        exp.canonical_name(),
                        ty.canonical_name()
                    ));
                }
                self.locals.insert(name.clone(), ty);
                out.push(KernelStmt::Let {
                    name: name.clone(),
                    ty,
                    expr,
                });
                Ok(())
            }
            WitnessKind::Assign { target, value } => {
                let Some(&target_ty) = self.locals.get(target) else {
                    return Err(format!(
                        "kernel '{}': cannot assign to undeclared local '{}' (parameters are read-only; declare with 'let')",
                        self.kernel_name, target
                    ));
                };
                let expr = self.lower_expr(value, Some(target_ty))?;
                if expr.ty() != target_ty {
                    return Err(format!(
                        "kernel '{}': cannot assign {} to local '{}' of type {}",
                        self.kernel_name,
                        expr.ty().canonical_name(),
                        target,
                        target_ty.canonical_name()
                    ));
                }
                out.push(KernelStmt::Assign {
                    name: target.clone(),
                    expr,
                });
                Ok(())
            }
            WitnessKind::IndexAssign {
                object,
                index,
                value,
            } => {
                let WitnessKind::Variable(buf_name) = &object.kind else {
                    return Err(format!(
                        "kernel '{}': store target must be a buffer parameter name",
                        self.kernel_name
                    ));
                };
                let Some(&(buf_param, elem_ty)) = self.buf_params.get(buf_name) else {
                    return Err(format!(
                        "kernel '{}': '{}' is not a buffer parameter",
                        self.kernel_name, buf_name
                    ));
                };
                let idx_expr = self.lower_expr(index, Some(KernelScalarType::I32))?;
                if idx_expr.ty() != KernelScalarType::I32 {
                    return Err(format!(
                        "kernel '{}': buffer index into '{}' must be i32, got {}",
                        self.kernel_name,
                        buf_name,
                        idx_expr.ty().canonical_name()
                    ));
                }
                let val_expr = self.lower_expr(value, Some(elem_ty))?;
                if val_expr.ty() != elem_ty {
                    return Err(format!(
                        "kernel '{}': buffer '{}' expects {} elements, got {}",
                        self.kernel_name,
                        buf_name,
                        elem_ty.canonical_name(),
                        val_expr.ty().canonical_name()
                    ));
                }
                out.push(KernelStmt::Store {
                    buf_param,
                    index: idx_expr,
                    value: val_expr,
                });
                Ok(())
            }
            WitnessKind::If { cond, then, r#else } => {
                let cond_expr = self.lower_expr(cond, Some(KernelScalarType::Bool))?;
                if cond_expr.ty() != KernelScalarType::Bool {
                    return Err(format!(
                        "kernel '{}': 'if' condition must be bool, got {}",
                        self.kernel_name,
                        cond_expr.ty().canonical_name()
                    ));
                }
                let saved = self.locals.clone();
                let mut then_body = Vec::new();
                self.lower_stmt(then, &mut then_body)?;
                self.locals = saved.clone();
                let mut else_body = Vec::new();
                if let Some(el) = r#else {
                    self.lower_stmt(el, &mut else_body)?;
                }
                self.locals = saved;
                out.push(KernelStmt::If {
                    cond: cond_expr,
                    then_body,
                    else_body,
                });
                Ok(())
            }
            WitnessKind::While { cond, body } => {
                let cond_expr = self.lower_expr(cond, Some(KernelScalarType::Bool))?;
                if cond_expr.ty() != KernelScalarType::Bool {
                    return Err(format!(
                        "kernel '{}': 'while' condition must be bool, got {}",
                        self.kernel_name,
                        cond_expr.ty().canonical_name()
                    ));
                }
                let saved = self.locals.clone();
                let mut loop_body = Vec::new();
                self.lower_stmt(body, &mut loop_body)?;
                self.locals = saved;
                out.push(KernelStmt::While {
                    cond: cond_expr,
                    body: loop_body,
                });
                Ok(())
            }
            WitnessKind::Loop {
                var,
                iterable,
                body,
            } => {
                // Support `for i in range(stop)` and `for i in range(start, stop)` cleanly!
                let (start_expr, stop_expr) = match &iterable.kind {
                    WitnessKind::Call {
                        callee: WitnessCallee::Name(n) | WitnessCallee::Var(n),
                        args,
                        named_args,
                    } if n == "range"
                        && named_args.is_empty()
                        && (args.len() == 1 || args.len() == 2) =>
                    {
                        if args.len() == 1 {
                            (
                                KernelExpr::ConstI32(0),
                                self.lower_expr(&args[0], Some(KernelScalarType::I32))?,
                            )
                        } else {
                            (
                                self.lower_expr(&args[0], Some(KernelScalarType::I32))?,
                                self.lower_expr(&args[1], Some(KernelScalarType::I32))?,
                            )
                        }
                    }
                    _ => {
                        return Err(format!(
                            "kernel '{}': 'for' loops inside GPU kernels must iterate over range(stop) or range(start, stop)",
                            self.kernel_name
                        ));
                    }
                };
                if start_expr.ty() != KernelScalarType::I32
                    || stop_expr.ty() != KernelScalarType::I32
                {
                    return Err(format!(
                        "kernel '{}': range bounds in 'for' loop must be i32",
                        self.kernel_name
                    ));
                }
                let end_var = format!("__k_end_{var}");
                out.push(KernelStmt::Let {
                    name: var.clone(),
                    ty: KernelScalarType::I32,
                    expr: start_expr,
                });
                out.push(KernelStmt::Let {
                    name: end_var.clone(),
                    ty: KernelScalarType::I32,
                    expr: stop_expr,
                });
                let saved = self.locals.clone();
                self.locals.insert(var.clone(), KernelScalarType::I32);
                self.locals.insert(end_var.clone(), KernelScalarType::I32);
                let mut loop_body = Vec::new();
                self.lower_stmt(body, &mut loop_body)?;
                loop_body.push(KernelStmt::Assign {
                    name: var.clone(),
                    expr: KernelExpr::Binary {
                        op: KernelBinOp::Add,
                        lhs: Box::new(KernelExpr::Var(var.clone(), KernelScalarType::I32)),
                        rhs: Box::new(KernelExpr::ConstI32(1)),
                        ty: KernelScalarType::I32,
                    },
                });
                self.locals = saved;
                out.push(KernelStmt::While {
                    cond: KernelExpr::Binary {
                        op: KernelBinOp::Lt,
                        lhs: Box::new(KernelExpr::Var(var.clone(), KernelScalarType::I32)),
                        rhs: Box::new(KernelExpr::Var(end_var, KernelScalarType::I32)),
                        ty: KernelScalarType::Bool,
                    },
                    body: loop_body,
                });
                Ok(())
            }
            other => Err(format!(
                "kernel '{}': unsupported statement form in GPU kernel body ({:?})",
                self.kernel_name,
                std::mem::discriminant(other)
            )),
        }
    }

    fn lower_expr(
        &self,
        w: &MirWitness,
        expected: Option<KernelScalarType>,
    ) -> Result<KernelExpr, String> {
        match &w.kind {
            WitnessKind::Literal(lit) => match lit {
                Literal::Int(n, _) | Literal::TypedInt(n, _, _) => {
                    Ok(KernelExpr::ConstI32(*n as i32))
                }
                Literal::Float(v, _) => {
                    if expected == Some(KernelScalarType::F32) {
                        Ok(KernelExpr::ConstF32(*v as f32))
                    } else {
                        Ok(KernelExpr::ConstF64(*v))
                    }
                }
                Literal::TypedFloat(v, DType::F32, _) => Ok(KernelExpr::ConstF32(*v as f32)),
                Literal::TypedFloat(v, _, _) => Ok(KernelExpr::ConstF64(*v)),
                Literal::Bool(b, _) => Ok(KernelExpr::ConstBool(*b)),
                other => Err(format!(
                    "kernel '{}': unsupported literal '{other:?}' in GPU kernel",
                    self.kernel_name
                )),
            },
            WitnessKind::Variable(name) => {
                if let Some(&ty) = self.locals.get(name) {
                    return Ok(KernelExpr::Var(name.clone(), ty));
                }
                if let Some(&(idx, ty)) = self.scalar_params.get(name) {
                    return Ok(KernelExpr::Param(idx, ty));
                }
                if self.buf_params.contains_key(name) {
                    return Err(format!(
                        "kernel '{}': buffer parameter '{}' must be indexed as '{}[idx]'",
                        self.kernel_name, name, name
                    ));
                }
                Err(format!(
                    "kernel '{}': undefined variable '{}' in GPU kernel",
                    self.kernel_name, name
                ))
            }
            WitnessKind::Or { left, right } => {
                let l = self.lower_expr(left, Some(KernelScalarType::Bool))?;
                let r = self.lower_expr(right, Some(KernelScalarType::Bool))?;
                if l.ty() != KernelScalarType::Bool || r.ty() != KernelScalarType::Bool {
                    return Err(format!(
                        "kernel '{}': 'or' requires bool operands",
                        self.kernel_name
                    ));
                }
                Ok(KernelExpr::Binary {
                    op: KernelBinOp::Or,
                    lhs: Box::new(l),
                    rhs: Box::new(r),
                    ty: KernelScalarType::Bool,
                })
            }
            WitnessKind::And { left, right } => {
                let l = self.lower_expr(left, Some(KernelScalarType::Bool))?;
                let r = self.lower_expr(right, Some(KernelScalarType::Bool))?;
                if l.ty() != KernelScalarType::Bool || r.ty() != KernelScalarType::Bool {
                    return Err(format!(
                        "kernel '{}': 'and' requires bool operands",
                        self.kernel_name
                    ));
                }
                Ok(KernelExpr::Binary {
                    op: KernelBinOp::And,
                    lhs: Box::new(l),
                    rhs: Box::new(r),
                    ty: KernelScalarType::Bool,
                })
            }
            WitnessKind::Binary { left, op, right } => {
                self.lower_binary(left, *op, right, expected)
            }
            WitnessKind::Call {
                callee,
                args,
                named_args,
            } => {
                if !named_args.is_empty() {
                    return Err(format!(
                        "kernel '{}': keyword arguments are not supported inside GPU kernels",
                        self.kernel_name
                    ));
                }
                match callee {
                    WitnessCallee::Builtin(BuiltinOp::Neg) if args.len() == 1 => {
                        let inner = self.lower_expr(&args[0], expected)?;
                        let ty = inner.ty();
                        if ty == KernelScalarType::Bool {
                            return Err(format!(
                                "kernel '{}': unary '-' cannot negate bool",
                                self.kernel_name
                            ));
                        }
                        Ok(KernelExpr::Unary {
                            op: KernelUnaryOp::Neg,
                            arg: Box::new(inner),
                            ty,
                        })
                    }
                    WitnessCallee::Builtin(BuiltinOp::Not) if args.len() == 1 => {
                        let inner = self.lower_expr(&args[0], Some(KernelScalarType::Bool))?;
                        if inner.ty() != KernelScalarType::Bool {
                            return Err(format!(
                                "kernel '{}': unary '!' / 'not' requires bool operand",
                                self.kernel_name
                            ));
                        }
                        Ok(KernelExpr::Unary {
                            op: KernelUnaryOp::Not,
                            arg: Box::new(inner),
                            ty: KernelScalarType::Bool,
                        })
                    }
                    WitnessCallee::Name(n) | WitnessCallee::Var(n) if n == "[]" => {
                        if args.len() != 2 {
                            return Err(format!(
                                "kernel '{}': buffer load expects 1 index",
                                self.kernel_name
                            ));
                        }
                        let WitnessKind::Variable(buf_name) = &args[0].kind else {
                            return Err(format!(
                                "kernel '{}': index target must be a buffer parameter",
                                self.kernel_name
                            ));
                        };
                        let Some(&(buf_param, elem_ty)) = self.buf_params.get(buf_name) else {
                            return Err(format!(
                                "kernel '{}': '{}' is not a buffer parameter",
                                self.kernel_name, buf_name
                            ));
                        };
                        let idx_expr = self.lower_expr(&args[1], Some(KernelScalarType::I32))?;
                        if idx_expr.ty() != KernelScalarType::I32 {
                            return Err(format!(
                                "kernel '{}': buffer index into '{}' must be i32, got {}",
                                self.kernel_name,
                                buf_name,
                                idx_expr.ty().canonical_name()
                            ));
                        }
                        Ok(KernelExpr::Load {
                            buf_param,
                            index: Box::new(idx_expr),
                            elem_ty,
                        })
                    }
                    WitnessCallee::Name(n) | WitnessCallee::Var(n) => {
                        self.lower_intrinsic_call(n, args, expected)
                    }
                    _ => Err(format!(
                        "kernel '{}': dynamic function calls are not allowed inside GPU kernels",
                        self.kernel_name
                    )),
                }
            }
            WitnessKind::MethodCall {
                receiver,
                method,
                args,
                named_args,
            } => {
                if !named_args.is_empty() {
                    return Err(format!(
                        "kernel '{}': keyword arguments are not supported inside GPU kernels",
                        self.kernel_name
                    ));
                }
                if matches!(&receiver.kind, WitnessKind::Variable(m) if m == "math") {
                    return self.lower_intrinsic_call(method, args, expected);
                }
                Err(format!(
                    "kernel '{}': unsupported method call '.{}' in GPU kernel",
                    self.kernel_name, method
                ))
            }
            other => Err(format!(
                "kernel '{}': unsupported expression in GPU kernel ({:?})",
                self.kernel_name,
                std::mem::discriminant(other)
            )),
        }
    }

    fn lower_intrinsic_call(
        &self,
        name: &str,
        args: &[MirWitness],
        expected: Option<KernelScalarType>,
    ) -> Result<KernelExpr, String> {
        match name {
            "global_id" if args.is_empty() => Ok(KernelExpr::GlobalId),
            "block_idx" if args.is_empty() => Ok(KernelExpr::BlockIdx),
            "thread_idx" if args.is_empty() => Ok(KernelExpr::ThreadIdx),
            "block_dim" if args.is_empty() => Ok(KernelExpr::BlockDim),
            "grid_dim" if args.is_empty() => Ok(KernelExpr::GridDim),
            "exp" | "sqrt" | "tanh" if args.len() == 1 => {
                let inner = self.lower_expr(&args[0], expected)?;
                if !inner.ty().is_float() {
                    return Err(format!(
                        "kernel '{}': '{name}(x)' requires f32 or f64 argument, got {}",
                        self.kernel_name,
                        inner.ty().canonical_name()
                    ));
                }
                let op = match name {
                    "exp" => KernelUnaryOp::Exp,
                    "sqrt" => KernelUnaryOp::Sqrt,
                    "tanh" => KernelUnaryOp::Tanh,
                    _ => unreachable!(),
                };
                let ty = inner.ty();
                Ok(KernelExpr::Unary {
                    op,
                    arg: Box::new(inner),
                    ty,
                })
            }
            "abs" if args.len() == 1 => {
                let inner = self.lower_expr(&args[0], expected)?;
                let ty = inner.ty();
                if ty == KernelScalarType::Bool {
                    return Err(format!(
                        "kernel '{}': 'abs(x)' requires numeric argument",
                        self.kernel_name
                    ));
                }
                Ok(KernelExpr::Unary {
                    op: KernelUnaryOp::Abs,
                    arg: Box::new(inner),
                    ty,
                })
            }
            "f32" if args.len() == 1 => {
                let inner = self.lower_expr(&args[0], Some(KernelScalarType::F32))?;
                if inner.ty() == KernelScalarType::F32 {
                    Ok(inner)
                } else if inner.ty() == KernelScalarType::Bool {
                    Err(format!(
                        "kernel '{}': cannot cast bool to f32",
                        self.kernel_name
                    ))
                } else {
                    Ok(KernelExpr::Unary {
                        op: KernelUnaryOp::ToF32,
                        arg: Box::new(inner),
                        ty: KernelScalarType::F32,
                    })
                }
            }
            "f64" | "float" if args.len() == 1 => {
                let inner = self.lower_expr(&args[0], Some(KernelScalarType::F64))?;
                if inner.ty() == KernelScalarType::F64 {
                    Ok(inner)
                } else if inner.ty() == KernelScalarType::Bool {
                    Err(format!(
                        "kernel '{}': cannot cast bool to f64",
                        self.kernel_name
                    ))
                } else {
                    Ok(KernelExpr::Unary {
                        op: KernelUnaryOp::ToF64,
                        arg: Box::new(inner),
                        ty: KernelScalarType::F64,
                    })
                }
            }
            "i32" | "int" if args.len() == 1 => {
                let inner = self.lower_expr(&args[0], Some(KernelScalarType::I32))?;
                if inner.ty() == KernelScalarType::I32 {
                    Ok(inner)
                } else if inner.ty() == KernelScalarType::Bool {
                    Err(format!(
                        "kernel '{}': cannot cast bool to i32",
                        self.kernel_name
                    ))
                } else {
                    Ok(KernelExpr::Unary {
                        op: KernelUnaryOp::ToI32,
                        arg: Box::new(inner),
                        ty: KernelScalarType::I32,
                    })
                }
            }
            "max" | "min" if args.len() == 2 => {
                let (l, r) = self.lower_matched_pair(&args[0], &args[1], expected)?;
                if l.ty() == KernelScalarType::Bool {
                    return Err(format!(
                        "kernel '{}': '{name}' requires numeric operands",
                        self.kernel_name
                    ));
                }
                let ty = l.ty();
                Ok(KernelExpr::Binary {
                    op: if name == "max" {
                        KernelBinOp::Max
                    } else {
                        KernelBinOp::Min
                    },
                    lhs: Box::new(l),
                    rhs: Box::new(r),
                    ty,
                })
            }
            other => Err(format!(
                "kernel '{}': unknown GPU kernel intrinsic '{other}' (arity {})",
                self.kernel_name,
                args.len()
            )),
        }
    }

    fn lower_matched_pair(
        &self,
        left: &MirWitness,
        right: &MirWitness,
        expected: Option<KernelScalarType>,
    ) -> Result<(KernelExpr, KernelExpr), String> {
        let left_is_untyped_float = matches!(left.kind, WitnessKind::Literal(Literal::Float(_, _)));
        let right_is_untyped_float =
            matches!(right.kind, WitnessKind::Literal(Literal::Float(_, _)));

        if left_is_untyped_float && !right_is_untyped_float {
            let r = self.lower_expr(right, expected)?;
            let l = self.lower_expr(left, Some(r.ty()))?;
            if l.ty() != r.ty() {
                return Err(format!(
                    "kernel '{}': operand type mismatch ({} vs {})",
                    self.kernel_name,
                    l.ty().canonical_name(),
                    r.ty().canonical_name()
                ));
            }
            return Ok((l, r));
        }
        let l = self.lower_expr(left, expected)?;
        let r_expected = if right_is_untyped_float {
            Some(l.ty())
        } else {
            expected
        };
        let r = self.lower_expr(right, r_expected)?;
        if l.ty() != r.ty() {
            return Err(format!(
                "kernel '{}': operand type mismatch ({} vs {})",
                self.kernel_name,
                l.ty().canonical_name(),
                r.ty().canonical_name()
            ));
        }
        Ok((l, r))
    }

    fn lower_binary(
        &self,
        left: &MirWitness,
        op: BinaryOp,
        right: &MirWitness,
        expected: Option<KernelScalarType>,
    ) -> Result<KernelExpr, String> {
        let arith_hint = match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                expected
            }
            _ => None,
        };
        let (l, r) = self.lower_matched_pair(left, right, arith_hint)?;
        let operand_ty = l.ty();
        let (kop, out_ty) = match op {
            BinaryOp::Add => (KernelBinOp::Add, operand_ty),
            BinaryOp::Sub => (KernelBinOp::Sub, operand_ty),
            BinaryOp::Mul => (KernelBinOp::Mul, operand_ty),
            BinaryOp::Div => (KernelBinOp::Div, operand_ty),
            BinaryOp::Mod => {
                if operand_ty != KernelScalarType::I32 {
                    return Err(format!(
                        "kernel '{}': '%' remainder requires i32 operands, got {}",
                        self.kernel_name,
                        operand_ty.canonical_name()
                    ));
                }
                (KernelBinOp::Rem, KernelScalarType::I32)
            }
            BinaryOp::Less => (KernelBinOp::Lt, KernelScalarType::Bool),
            BinaryOp::LessEqual => (KernelBinOp::Le, KernelScalarType::Bool),
            BinaryOp::Greater => (KernelBinOp::Gt, KernelScalarType::Bool),
            BinaryOp::GreaterEqual => (KernelBinOp::Ge, KernelScalarType::Bool),
            BinaryOp::Equal => (KernelBinOp::Eq, KernelScalarType::Bool),
            BinaryOp::NotEqual => (KernelBinOp::Ne, KernelScalarType::Bool),
            other => {
                return Err(format!(
                    "kernel '{}': operator '{other:?}' is not supported inside GPU kernels",
                    self.kernel_name
                ));
            }
        };
        if matches!(
            kop,
            KernelBinOp::Add
                | KernelBinOp::Sub
                | KernelBinOp::Mul
                | KernelBinOp::Div
                | KernelBinOp::Lt
                | KernelBinOp::Le
                | KernelBinOp::Gt
                | KernelBinOp::Ge
        ) && operand_ty == KernelScalarType::Bool
        {
            return Err(format!(
                "kernel '{}': operator '{op:?}' requires numeric operands",
                self.kernel_name
            ));
        }
        Ok(KernelExpr::Binary {
            op: kop,
            lhs: Box::new(l),
            rhs: Box::new(r),
            ty: out_ty,
        })
    }
}

fn hint_to_kernel_scalar(
    ty: &crate::typeck::Type,
    kernel_name: &str,
    local_name: &str,
) -> Result<KernelScalarType, String> {
    match ty {
        crate::typeck::Type::Int | crate::typeck::Type::Scalar(DType::I32) => {
            Ok(KernelScalarType::I32)
        }
        crate::typeck::Type::Scalar(DType::F32) => Ok(KernelScalarType::F32),
        crate::typeck::Type::Float => Ok(KernelScalarType::F64),
        crate::typeck::Type::Bool => Ok(KernelScalarType::Bool),
        other => Err(format!(
            "kernel '{kernel_name}': local '{local_name}' type '{}' is not supported in GPU kernels (expected i32, f32, f64, or bool)",
            other.name()
        )),
    }
}

/// Lower & statically type-check a parsed `WitnessKind::KernelDef` into a typed [`KernelProgram`].
pub(crate) fn lower_witness_kernel(
    name: &str,
    params: &[KernelParam],
    body: &[MirWitness],
) -> Result<KernelProgram, String> {
    let mut ctx = KernelLowerCtx::new(name, params);
    let stmts = ctx.lower_block(body)?;
    Ok(KernelProgram {
        name: name.to_string(),
        params: params.to_vec(),
        body: stmts,
    })
}

// ============================================================================
// 2. Backend 1: CUDA PTX ISA 7.0 Emitter (Pure ASCII, sm_50+)
// ============================================================================

struct PtxEmitter {
    body: String,
    n_pred: u32,
    n_s32: u32,
    n_u64: u32,
    n_f32: u32,
    n_f64: u32,
    n_label: u32,
    param_regs: Vec<String>,
    local_regs: HashMap<String, (KernelScalarType, String)>,
}

impl PtxEmitter {
    fn new() -> Self {
        Self {
            body: String::with_capacity(1024),
            n_pred: 1,
            n_s32: 1,
            n_u64: 1,
            n_f32: 1,
            n_f64: 1,
            n_label: 0,
            param_regs: Vec::new(),
            local_regs: HashMap::new(),
        }
    }

    fn alloc_pred(&mut self) -> String {
        let id = self.n_pred;
        self.n_pred += 1;
        format!("%p{id}")
    }

    fn alloc_s32(&mut self) -> String {
        let id = self.n_s32;
        self.n_s32 += 1;
        format!("%r{id}")
    }

    fn alloc_u64(&mut self) -> String {
        let id = self.n_u64;
        self.n_u64 += 1;
        format!("%rd{id}")
    }

    fn alloc_f32(&mut self) -> String {
        let id = self.n_f32;
        self.n_f32 += 1;
        format!("%f{id}")
    }

    fn alloc_f64(&mut self) -> String {
        let id = self.n_f64;
        self.n_f64 += 1;
        format!("%fd{id}")
    }

    fn alloc_for_ty(&mut self, ty: KernelScalarType) -> String {
        match ty {
            KernelScalarType::I32 => self.alloc_s32(),
            KernelScalarType::F32 => self.alloc_f32(),
            KernelScalarType::F64 => self.alloc_f64(),
            KernelScalarType::Bool => self.alloc_pred(),
        }
    }

    fn alloc_label(&mut self, prefix: &str) -> String {
        let id = self.n_label;
        self.n_label += 1;
        format!("$L_{prefix}_{id}")
    }

    fn emit_mov(&mut self, ty: KernelScalarType, dst: &str, src: &str) {
        let op = match ty {
            KernelScalarType::I32 => "mov.s32",
            KernelScalarType::F32 => "mov.f32",
            KernelScalarType::F64 => "mov.f64",
            KernelScalarType::Bool => "mov.pred",
        };
        let _ = writeln!(self.body, "    {op} {dst}, {src};");
    }

    fn emit_stmts(&mut self, stmts: &[KernelStmt]) {
        for s in stmts {
            match s {
                KernelStmt::Let { name, ty, expr } => {
                    let val = self.emit_expr(expr);
                    let reg = if let Some((_, existing)) = self.local_regs.get(name) {
                        existing.clone()
                    } else {
                        let r = self.alloc_for_ty(*ty);
                        self.local_regs.insert(name.clone(), (*ty, r.clone()));
                        r
                    };
                    self.emit_mov(*ty, &reg, &val);
                }
                KernelStmt::Assign { name, expr } => {
                    let val = self.emit_expr(expr);
                    if let Some((ty, reg)) = self.local_regs.get(name).cloned() {
                        self.emit_mov(ty, &reg, &val);
                    }
                }
                KernelStmt::Store {
                    buf_param,
                    index,
                    value,
                } => {
                    let idx_reg = self.emit_expr(index);
                    let val_reg = self.emit_expr(value);
                    let base = self.param_regs[*buf_param].clone();
                    let off = self.alloc_u64();
                    let ptr = self.alloc_u64();
                    let (stride, st_op) = match value.ty() {
                        KernelScalarType::F32 => (4, "st.global.f32"),
                        KernelScalarType::F64 => (8, "st.global.f64"),
                        _ => (4, "st.global.s32"),
                    };
                    let _ = writeln!(self.body, "    mul.wide.s32 {off}, {idx_reg}, {stride};");
                    let _ = writeln!(self.body, "    add.u64 {ptr}, {base}, {off};");
                    let _ = writeln!(self.body, "    {st_op} [{ptr}], {val_reg};");
                }
                KernelStmt::If {
                    cond,
                    then_body,
                    else_body,
                } => {
                    let cond_reg = self.emit_expr(cond);
                    let else_lbl = self.alloc_label("else");
                    let end_lbl = self.alloc_label("endif");
                    if else_body.is_empty() {
                        let _ = writeln!(self.body, "    @!{cond_reg} bra {end_lbl};");
                        self.emit_stmts(then_body);
                        let _ = writeln!(self.body, "{end_lbl}:");
                    } else {
                        let _ = writeln!(self.body, "    @!{cond_reg} bra {else_lbl};");
                        self.emit_stmts(then_body);
                        let _ = writeln!(self.body, "    bra {end_lbl};");
                        let _ = writeln!(self.body, "{else_lbl}:");
                        self.emit_stmts(else_body);
                        let _ = writeln!(self.body, "{end_lbl}:");
                    }
                }
                KernelStmt::While { cond, body } => {
                    let top_lbl = self.alloc_label("while_top");
                    let end_lbl = self.alloc_label("while_end");
                    let _ = writeln!(self.body, "{top_lbl}:");
                    let cond_reg = self.emit_expr(cond);
                    let _ = writeln!(self.body, "    @!{cond_reg} bra {end_lbl};");
                    self.emit_stmts(body);
                    let _ = writeln!(self.body, "    bra {top_lbl};");
                    let _ = writeln!(self.body, "{end_lbl}:");
                }
            }
        }
    }

    fn emit_exp_f32(&mut self, arg: &str) -> String {
        let log2e = self.alloc_f32();
        let scaled = self.alloc_f32();
        let out = self.alloc_f32();
        let _ = writeln!(
            self.body,
            "    mov.f32 {log2e}, 0f{:08X};",
            std::f32::consts::LOG2_E.to_bits()
        );
        let _ = writeln!(self.body, "    mul.f32 {scaled}, {arg}, {log2e};");
        let _ = writeln!(self.body, "    ex2.approx.f32 {out}, {scaled};");
        out
    }

    fn emit_exp_f64(&mut self, arg: &str) -> String {
        // Range-reduced degree-6 Horner minimax exp(x) = 2^k * exp(r) in pure PTX f64.
        let log2e = self.alloc_f64();
        let ln2 = self.alloc_f64();
        let xf = self.alloc_f64();
        let k_i32 = self.alloc_s32();
        let k_f64 = self.alloc_f64();
        let r = self.alloc_f64();
        let _ = writeln!(
            self.body,
            "    mov.f64 {log2e}, 0d{:016X};",
            std::f64::consts::LOG2_E.to_bits()
        );
        let _ = writeln!(
            self.body,
            "    mov.f64 {ln2}, 0d{:016X};",
            std::f64::consts::LN_2.to_bits()
        );
        let _ = writeln!(self.body, "    mul.rn.f64 {xf}, {arg}, {log2e};");
        let _ = writeln!(self.body, "    cvt.rni.s32.f64 {k_i32}, {xf};");
        let _ = writeln!(self.body, "    cvt.rn.f64.s32 {k_f64}, {k_i32};");
        let _ = writeln!(self.body, "    mul.rn.f64 {r}, {k_f64}, {ln2};");
        let _ = writeln!(self.body, "    sub.rn.f64 {r}, {arg}, {r};");

        let coeffs: [f64; 6] = [1.0 / 720.0, 1.0 / 120.0, 1.0 / 24.0, 1.0 / 6.0, 0.5, 1.0];
        let acc = self.alloc_f64();
        let _ = writeln!(
            self.body,
            "    mov.f64 {acc}, 0d{:016X};",
            coeffs[0].to_bits()
        );
        let ctmp = self.alloc_f64();
        for &c in &coeffs[1..] {
            let _ = writeln!(self.body, "    mov.f64 {ctmp}, 0d{:016X};", c.to_bits());
            let _ = writeln!(self.body, "    fma.rn.f64 {acc}, {acc}, {r}, {ctmp};");
        }
        let one = self.alloc_f64();
        let _ = writeln!(
            self.body,
            "    mov.f64 {one}, 0d{:016X};",
            1.0_f64.to_bits()
        );
        let _ = writeln!(self.body, "    fma.rn.f64 {acc}, {acc}, {r}, {one};");

        // Build 2^k via IEEE-754 exponent bias: ((clamp(k, -1022, 1023) + 1023) << 52).
        let k_clamped = self.alloc_s32();
        let _ = writeln!(self.body, "    max.s32 {k_clamped}, {k_i32}, -1022;");
        let _ = writeln!(self.body, "    min.s32 {k_clamped}, {k_clamped}, 1023;");
        let _ = writeln!(self.body, "    add.s32 {k_clamped}, {k_clamped}, 1023;");
        let exp_u64 = self.alloc_u64();
        let pow2_f64 = self.alloc_f64();
        let out = self.alloc_f64();
        let _ = writeln!(self.body, "    cvt.u64.s32 {exp_u64}, {k_clamped};");
        let _ = writeln!(self.body, "    shl.b64 {exp_u64}, {exp_u64}, 52;");
        let _ = writeln!(self.body, "    mov.b64 {pow2_f64}, {exp_u64};");
        let _ = writeln!(self.body, "    mul.rn.f64 {out}, {acc}, {pow2_f64};");
        out
    }

    fn emit_expr(&mut self, expr: &KernelExpr) -> String {
        match expr {
            KernelExpr::ConstI32(v) => {
                let r = self.alloc_s32();
                let _ = writeln!(self.body, "    mov.s32 {r}, {v};");
                r
            }
            KernelExpr::ConstF32(v) => {
                let r = self.alloc_f32();
                let _ = writeln!(self.body, "    mov.f32 {r}, 0f{:08X};", v.to_bits());
                r
            }
            KernelExpr::ConstF64(v) => {
                let r = self.alloc_f64();
                let _ = writeln!(self.body, "    mov.f64 {r}, 0d{:016X};", v.to_bits());
                r
            }
            KernelExpr::ConstBool(b) => {
                let p = self.alloc_pred();
                let _ = writeln!(self.body, "    setp.ne.s32 {p}, {}, 0;", i32::from(*b));
                p
            }
            KernelExpr::Param(idx, _) => self.param_regs[*idx].clone(),
            KernelExpr::Var(name, _) => self
                .local_regs
                .get(name)
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| "%r0".to_string()),
            KernelExpr::GlobalId => {
                let b = self.alloc_s32();
                let d = self.alloc_s32();
                let t = self.alloc_s32();
                let out = self.alloc_s32();
                let _ = writeln!(self.body, "    mov.u32 {b}, %ctaid.x;");
                let _ = writeln!(self.body, "    mov.u32 {d}, %ntid.x;");
                let _ = writeln!(self.body, "    mov.u32 {t}, %tid.x;");
                let _ = writeln!(self.body, "    mad.lo.s32 {out}, {b}, {d}, {t};");
                out
            }
            KernelExpr::BlockIdx => {
                let r = self.alloc_s32();
                let _ = writeln!(self.body, "    mov.u32 {r}, %ctaid.x;");
                r
            }
            KernelExpr::ThreadIdx => {
                let r = self.alloc_s32();
                let _ = writeln!(self.body, "    mov.u32 {r}, %tid.x;");
                r
            }
            KernelExpr::BlockDim => {
                let r = self.alloc_s32();
                let _ = writeln!(self.body, "    mov.u32 {r}, %ntid.x;");
                r
            }
            KernelExpr::GridDim => {
                let r = self.alloc_s32();
                let _ = writeln!(self.body, "    mov.u32 {r}, %nctaid.x;");
                r
            }
            KernelExpr::Load {
                buf_param,
                index,
                elem_ty,
            } => {
                let idx_reg = self.emit_expr(index);
                let base = self.param_regs[*buf_param].clone();
                let off = self.alloc_u64();
                let ptr = self.alloc_u64();
                let (stride, ld_op, dst) = match elem_ty {
                    KernelScalarType::F32 => (4, "ld.global.f32", self.alloc_f32()),
                    KernelScalarType::F64 => (8, "ld.global.f64", self.alloc_f64()),
                    _ => (4, "ld.global.s32", self.alloc_s32()),
                };
                let _ = writeln!(self.body, "    mul.wide.s32 {off}, {idx_reg}, {stride};");
                let _ = writeln!(self.body, "    add.u64 {ptr}, {base}, {off};");
                let _ = writeln!(self.body, "    {ld_op} {dst}, [{ptr}];");
                dst
            }
            KernelExpr::Unary { op, arg, ty } => {
                let src = self.emit_expr(arg);
                match op {
                    KernelUnaryOp::Neg => {
                        let dst = self.alloc_for_ty(*ty);
                        let inst = match ty {
                            KernelScalarType::I32 => "neg.s32",
                            KernelScalarType::F32 => "neg.f32",
                            KernelScalarType::F64 => "neg.f64",
                            KernelScalarType::Bool => "not.pred",
                        };
                        let _ = writeln!(self.body, "    {inst} {dst}, {src};");
                        dst
                    }
                    KernelUnaryOp::Not => {
                        let dst = self.alloc_pred();
                        let _ = writeln!(self.body, "    not.pred {dst}, {src};");
                        dst
                    }
                    KernelUnaryOp::Abs => {
                        let dst = self.alloc_for_ty(*ty);
                        let inst = match ty {
                            KernelScalarType::I32 => "abs.s32",
                            KernelScalarType::F32 => "abs.f32",
                            KernelScalarType::F64 => "abs.f64",
                            KernelScalarType::Bool => "mov.pred",
                        };
                        let _ = writeln!(self.body, "    {inst} {dst}, {src};");
                        dst
                    }
                    KernelUnaryOp::Sqrt => {
                        let dst = self.alloc_for_ty(*ty);
                        match ty {
                            KernelScalarType::F32 => {
                                let _ = writeln!(self.body, "    sqrt.approx.f32 {dst}, {src};");
                            }
                            KernelScalarType::F64 => {
                                let _ = writeln!(self.body, "    sqrt.rn.f64 {dst}, {src};");
                            }
                            _ => {}
                        }
                        dst
                    }
                    KernelUnaryOp::Exp => match ty {
                        KernelScalarType::F32 => self.emit_exp_f32(&src),
                        KernelScalarType::F64 => self.emit_exp_f64(&src),
                        _ => src,
                    },
                    KernelUnaryOp::Tanh => match ty {
                        KernelScalarType::F32 => {
                            let c = self.alloc_f32();
                            let lo = self.alloc_f32();
                            let hi = self.alloc_f32();
                            let two = self.alloc_f32();
                            let one = self.alloc_f32();
                            let twox = self.alloc_f32();
                            let num = self.alloc_f32();
                            let den = self.alloc_f32();
                            let out = self.alloc_f32();
                            let _ = writeln!(
                                self.body,
                                "    mov.f32 {lo}, 0f{:08X};",
                                (-10.0_f32).to_bits()
                            );
                            let _ = writeln!(
                                self.body,
                                "    mov.f32 {hi}, 0f{:08X};",
                                10.0_f32.to_bits()
                            );
                            let _ = writeln!(self.body, "    max.f32 {c}, {src}, {lo};");
                            let _ = writeln!(self.body, "    min.f32 {c}, {c}, {hi};");
                            let _ = writeln!(
                                self.body,
                                "    mov.f32 {two}, 0f{:08X};",
                                2.0_f32.to_bits()
                            );
                            let _ = writeln!(
                                self.body,
                                "    mov.f32 {one}, 0f{:08X};",
                                1.0_f32.to_bits()
                            );
                            let _ = writeln!(self.body, "    mul.f32 {twox}, {c}, {two};");
                            let e2 = self.emit_exp_f32(&twox);
                            let _ = writeln!(self.body, "    sub.f32 {num}, {e2}, {one};");
                            let _ = writeln!(self.body, "    add.f32 {den}, {e2}, {one};");
                            let _ = writeln!(self.body, "    div.rn.f32 {out}, {num}, {den};");
                            out
                        }
                        KernelScalarType::F64 => {
                            let c = self.alloc_f64();
                            let lo = self.alloc_f64();
                            let hi = self.alloc_f64();
                            let two = self.alloc_f64();
                            let one = self.alloc_f64();
                            let twox = self.alloc_f64();
                            let num = self.alloc_f64();
                            let den = self.alloc_f64();
                            let out = self.alloc_f64();
                            let _ = writeln!(
                                self.body,
                                "    mov.f64 {lo}, 0d{:016X};",
                                (-18.0_f64).to_bits()
                            );
                            let _ = writeln!(
                                self.body,
                                "    mov.f64 {hi}, 0d{:016X};",
                                18.0_f64.to_bits()
                            );
                            let _ = writeln!(self.body, "    max.f64 {c}, {src}, {lo};");
                            let _ = writeln!(self.body, "    min.f64 {c}, {c}, {hi};");
                            let _ = writeln!(
                                self.body,
                                "    mov.f64 {two}, 0d{:016X};",
                                2.0_f64.to_bits()
                            );
                            let _ = writeln!(
                                self.body,
                                "    mov.f64 {one}, 0d{:016X};",
                                1.0_f64.to_bits()
                            );
                            let _ = writeln!(self.body, "    mul.rn.f64 {twox}, {c}, {two};");
                            let e2 = self.emit_exp_f64(&twox);
                            let _ = writeln!(self.body, "    sub.rn.f64 {num}, {e2}, {one};");
                            let _ = writeln!(self.body, "    add.rn.f64 {den}, {e2}, {one};");
                            let _ = writeln!(self.body, "    div.rn.f64 {out}, {num}, {den};");
                            out
                        }
                        _ => src,
                    },
                    KernelUnaryOp::ToF32 => {
                        let dst = self.alloc_f32();
                        match arg.ty() {
                            KernelScalarType::I32 => {
                                let _ = writeln!(self.body, "    cvt.rn.f32.s32 {dst}, {src};");
                            }
                            KernelScalarType::F64 => {
                                let _ = writeln!(self.body, "    cvt.rn.f32.f64 {dst}, {src};");
                            }
                            _ => {
                                let _ = writeln!(self.body, "    mov.f32 {dst}, {src};");
                            }
                        }
                        dst
                    }
                    KernelUnaryOp::ToF64 => {
                        let dst = self.alloc_f64();
                        match arg.ty() {
                            KernelScalarType::I32 => {
                                let _ = writeln!(self.body, "    cvt.rn.f64.s32 {dst}, {src};");
                            }
                            KernelScalarType::F32 => {
                                let _ = writeln!(self.body, "    cvt.f64.f32 {dst}, {src};");
                            }
                            _ => {
                                let _ = writeln!(self.body, "    mov.f64 {dst}, {src};");
                            }
                        }
                        dst
                    }
                    KernelUnaryOp::ToI32 => {
                        let dst = self.alloc_s32();
                        match arg.ty() {
                            KernelScalarType::F32 => {
                                let _ = writeln!(self.body, "    cvt.rzi.s32.f32 {dst}, {src};");
                            }
                            KernelScalarType::F64 => {
                                let _ = writeln!(self.body, "    cvt.rzi.s32.f64 {dst}, {src};");
                            }
                            _ => {
                                let _ = writeln!(self.body, "    mov.s32 {dst}, {src};");
                            }
                        }
                        dst
                    }
                }
            }
            KernelExpr::Binary { op, lhs, rhs, ty } => {
                let l = self.emit_expr(lhs);
                let r = self.emit_expr(rhs);
                let lty = lhs.ty();
                match op {
                    KernelBinOp::Add
                    | KernelBinOp::Sub
                    | KernelBinOp::Mul
                    | KernelBinOp::Div
                    | KernelBinOp::Rem
                    | KernelBinOp::Min
                    | KernelBinOp::Max => {
                        let dst = self.alloc_for_ty(*ty);
                        let inst = match (op, lty) {
                            (KernelBinOp::Add, KernelScalarType::I32) => "add.s32",
                            (KernelBinOp::Add, KernelScalarType::F32) => "add.f32",
                            (KernelBinOp::Add, KernelScalarType::F64) => "add.rn.f64",
                            (KernelBinOp::Sub, KernelScalarType::I32) => "sub.s32",
                            (KernelBinOp::Sub, KernelScalarType::F32) => "sub.f32",
                            (KernelBinOp::Sub, KernelScalarType::F64) => "sub.rn.f64",
                            (KernelBinOp::Mul, KernelScalarType::I32) => "mul.lo.s32",
                            (KernelBinOp::Mul, KernelScalarType::F32) => "mul.f32",
                            (KernelBinOp::Mul, KernelScalarType::F64) => "mul.rn.f64",
                            (KernelBinOp::Div, KernelScalarType::I32) => "div.s32",
                            (KernelBinOp::Div, KernelScalarType::F32) => "div.rn.f32",
                            (KernelBinOp::Div, KernelScalarType::F64) => "div.rn.f64",
                            (KernelBinOp::Rem, KernelScalarType::I32) => "rem.s32",
                            (KernelBinOp::Min, KernelScalarType::I32) => "min.s32",
                            (KernelBinOp::Min, KernelScalarType::F32) => "min.f32",
                            (KernelBinOp::Min, KernelScalarType::F64) => "min.f64",
                            (KernelBinOp::Max, KernelScalarType::I32) => "max.s32",
                            (KernelBinOp::Max, KernelScalarType::F32) => "max.f32",
                            (KernelBinOp::Max, KernelScalarType::F64) => "max.f64",
                            _ => "add.s32",
                        };
                        let _ = writeln!(self.body, "    {inst} {dst}, {l}, {r};");
                        dst
                    }
                    KernelBinOp::Lt
                    | KernelBinOp::Le
                    | KernelBinOp::Gt
                    | KernelBinOp::Ge
                    | KernelBinOp::Eq
                    | KernelBinOp::Ne => {
                        let dst = self.alloc_pred();
                        let cmp = match op {
                            KernelBinOp::Lt => "lt",
                            KernelBinOp::Le => "le",
                            KernelBinOp::Gt => "gt",
                            KernelBinOp::Ge => "ge",
                            KernelBinOp::Eq => "eq",
                            KernelBinOp::Ne => "ne",
                            _ => unreachable!(),
                        };
                        match lty {
                            KernelScalarType::I32 => {
                                let _ = writeln!(self.body, "    setp.{cmp}.s32 {dst}, {l}, {r};");
                            }
                            KernelScalarType::F32 => {
                                let _ = writeln!(self.body, "    setp.{cmp}.f32 {dst}, {l}, {r};");
                            }
                            KernelScalarType::F64 => {
                                let _ = writeln!(self.body, "    setp.{cmp}.f64 {dst}, {l}, {r};");
                            }
                            KernelScalarType::Bool => {
                                let _ = writeln!(self.body, "    xor.pred {dst}, {l}, {r};");
                                if matches!(op, KernelBinOp::Eq) {
                                    let _ = writeln!(self.body, "    not.pred {dst}, {dst};");
                                }
                            }
                        }
                        dst
                    }
                    KernelBinOp::And => {
                        let dst = self.alloc_pred();
                        let _ = writeln!(self.body, "    and.pred {dst}, {l}, {r};");
                        dst
                    }
                    KernelBinOp::Or => {
                        let dst = self.alloc_pred();
                        let _ = writeln!(self.body, "    or.pred {dst}, {l}, {r};");
                        dst
                    }
                }
            }
        }
    }
}

/// Compile a typed [`KernelProgram`] to pure-ASCII CUDA PTX ISA 7.0 (`sm_50+`).
pub(crate) fn emit_ptx(program: &KernelProgram) -> String {
    let mut em = PtxEmitter::new();
    let mut param_decls = Vec::with_capacity(program.params.len());
    for (idx, p) in program.params.iter().enumerate() {
        let pname = format!("param_{idx}");
        let (ptx_ty, ld_op, reg) = match p.ty {
            KernelParamType::BufF32 | KernelParamType::BufF64 => {
                (".u64", "ld.param.u64", em.alloc_u64())
            }
            KernelParamType::F32 => (".f32", "ld.param.f32", em.alloc_f32()),
            KernelParamType::F64 => (".f64", "ld.param.f64", em.alloc_f64()),
            KernelParamType::I32 => (".u32", "ld.param.u32", em.alloc_s32()),
        };
        param_decls.push(format!("    .param {ptx_ty} {pname}"));
        let _ = writeln!(em.body, "    {ld_op} {reg}, [{pname}];");
        em.param_regs.push(reg);
    }
    em.emit_stmts(&program.body);
    let _ = writeln!(em.body, "    ret;");

    let mut out = String::with_capacity(em.body.len() + 256);
    let _ = writeln!(out, ".version 7.0");
    let _ = writeln!(out, ".target sm_50");
    let _ = writeln!(out, ".address_size 64");
    let _ = writeln!(out);
    let _ = writeln!(out, ".visible .entry {}(", program.name);
    let _ = writeln!(out, "{}", param_decls.join(",\n"));
    let _ = writeln!(out, ")");
    let _ = writeln!(out, "{{");
    let _ = writeln!(out, "    .reg .pred %p<{}>;", em.n_pred + 1);
    let _ = writeln!(out, "    .reg .s32 %r<{}>;", em.n_s32 + 1);
    let _ = writeln!(out, "    .reg .u64 %rd<{}>;", em.n_u64 + 1);
    let _ = writeln!(out, "    .reg .f32 %f<{}>;", em.n_f32 + 1);
    let _ = writeln!(out, "    .reg .f64 %fd<{}>;", em.n_f64 + 1);
    out.push_str(&em.body);
    let _ = writeln!(out, "}}");
    out
}

// ============================================================================
// 3. Backend 2: Vulkan SPIR-V 1.0 Emitter (GLCompute + GLSL.std.450)
// ============================================================================

const SPV_OP_EXT_INST_IMPORT: u16 = 11;
const SPV_OP_EXT_INST: u16 = 12;
const SPV_OP_MEMORY_MODEL: u16 = 14;
const SPV_OP_ENTRY_POINT: u16 = 15;
const SPV_OP_EXECUTION_MODE: u16 = 16;
const SPV_OP_CAPABILITY: u16 = 17;
const SPV_OP_TYPE_VOID: u16 = 19;
const SPV_OP_TYPE_BOOL: u16 = 20;
const SPV_OP_TYPE_INT: u16 = 21;
const SPV_OP_TYPE_FLOAT: u16 = 22;
const SPV_OP_TYPE_VECTOR: u16 = 23;
const SPV_OP_TYPE_RUNTIME_ARRAY: u16 = 29;
const SPV_OP_TYPE_STRUCT: u16 = 30;
const SPV_OP_TYPE_POINTER: u16 = 32;
const SPV_OP_TYPE_FUNCTION: u16 = 33;
const SPV_OP_CONSTANT_TRUE: u16 = 41;
const SPV_OP_CONSTANT_FALSE: u16 = 42;
const SPV_OP_CONSTANT: u16 = 43;
const SPV_OP_FUNCTION: u16 = 54;
const SPV_OP_FUNCTION_END: u16 = 56;
const SPV_OP_VARIABLE: u16 = 59;
const SPV_OP_LOAD: u16 = 61;
const SPV_OP_STORE: u16 = 62;
const SPV_OP_ACCESS_CHAIN: u16 = 65;
const SPV_OP_DECORATE: u16 = 71;
const SPV_OP_MEMBER_DECORATE: u16 = 72;
const SPV_OP_COMPOSITE_EXTRACT: u16 = 81;
const SPV_OP_CONVERT_F_TO_S: u16 = 110;
const SPV_OP_CONVERT_S_TO_F: u16 = 111;
const SPV_OP_FCONVERT: u16 = 115;
const SPV_OP_BITCAST: u16 = 124;
const SPV_OP_SNEGATE: u16 = 126;
const SPV_OP_FNEGATE: u16 = 127;
const SPV_OP_IADD: u16 = 128;
const SPV_OP_FADD: u16 = 129;
const SPV_OP_ISUB: u16 = 130;
const SPV_OP_FSUB: u16 = 131;
const SPV_OP_IMUL: u16 = 132;
const SPV_OP_FMUL: u16 = 133;
const SPV_OP_SDIV: u16 = 135;
const SPV_OP_FDIV: u16 = 136;
const SPV_OP_SREM: u16 = 138;
const SPV_OP_LOGICAL_EQUAL: u16 = 164;
const SPV_OP_LOGICAL_NOT_EQUAL: u16 = 165;
const SPV_OP_LOGICAL_OR: u16 = 166;
const SPV_OP_LOGICAL_AND: u16 = 167;
const SPV_OP_LOGICAL_NOT: u16 = 168;
const SPV_OP_IEQUAL: u16 = 170;
const SPV_OP_INOT_EQUAL: u16 = 171;
const SPV_OP_SGREATER_THAN: u16 = 173;
const SPV_OP_SGREATER_THAN_EQUAL: u16 = 175;
const SPV_OP_SLESS_THAN: u16 = 177;
const SPV_OP_SLESS_THAN_EQUAL: u16 = 179;
const SPV_OP_FORD_EQUAL: u16 = 180;
const SPV_OP_FORD_NOT_EQUAL: u16 = 182;
const SPV_OP_FORD_LESS_THAN: u16 = 184;
const SPV_OP_FORD_GREATER_THAN: u16 = 186;
const SPV_OP_FORD_LESS_THAN_EQUAL: u16 = 188;
const SPV_OP_FORD_GREATER_THAN_EQUAL: u16 = 190;
const SPV_OP_SHIFT_LEFT_LOGICAL: u16 = 196;
const SPV_OP_BITWISE_OR: u16 = 197;
const SPV_OP_UCONVERT: u16 = 114;
const SPV_OP_LOOP_MERGE: u16 = 246;
const SPV_OP_SELECTION_MERGE: u16 = 247;
const SPV_OP_LABEL: u16 = 248;
const SPV_OP_BRANCH: u16 = 249;
const SPV_OP_BRANCH_CONDITIONAL: u16 = 250;
const SPV_OP_RETURN: u16 = 253;

const GLSL_FABS: u32 = 4;
const GLSL_SABS: u32 = 5;
const GLSL_TANH: u32 = 21;
const GLSL_EXP: u32 = 27;
const GLSL_SQRT: u32 = 31;
const GLSL_FMIN: u32 = 37;
const GLSL_SMIN: u32 = 39;
const GLSL_FMAX: u32 = 40;
const GLSL_SMAX: u32 = 42;

const SPV_SC_INPUT: u32 = 1;
const SPV_SC_UNIFORM: u32 = 2;
const SPV_SC_FUNCTION: u32 = 7;

const SPV_DEC_BUFFER_BLOCK: u32 = 3;
const SPV_DEC_ARRAY_STRIDE: u32 = 6;
const SPV_DEC_BUILT_IN: u32 = 11;
const SPV_DEC_BINDING: u32 = 33;
const SPV_DEC_DESCRIPTOR_SET: u32 = 34;
const SPV_DEC_OFFSET: u32 = 35;

/// Packing layout for scalar arguments into Vulkan binding `n_bufs` (`uint[]` storage buffer).
pub(crate) struct VulkanKernelLayout {
    /// `(param_index, binding_index)` for every buffer parameter in order.
    pub buf_bindings: Vec<(usize, u32)>,
    /// `(param_index, word_offset)` inside the trailing scalar `uint[]` buffer (1 word for f32/i32, 2 words for f64).
    pub scalar_word_offsets: HashMap<usize, u32>,
    pub total_scalar_words: usize,
    pub total_bindings: u32,
}

pub(crate) fn vulkan_kernel_layout(program: &KernelProgram) -> VulkanKernelLayout {
    let mut buf_bindings = Vec::new();
    let mut scalar_word_offsets = HashMap::new();
    let mut next_binding = 0u32;
    let mut next_word = 0u32;
    for (idx, p) in program.params.iter().enumerate() {
        match p.ty {
            KernelParamType::BufF32 | KernelParamType::BufF64 => {
                buf_bindings.push((idx, next_binding));
                next_binding += 1;
            }
            KernelParamType::F32 | KernelParamType::I32 => {
                scalar_word_offsets.insert(idx, next_word);
                next_word += 1;
            }
            KernelParamType::F64 => {
                scalar_word_offsets.insert(idx, next_word);
                next_word += 2;
            }
        }
    }
    let total_bindings = if next_word > 0 {
        next_binding + 1
    } else {
        next_binding
    };
    VulkanKernelLayout {
        buf_bindings,
        scalar_word_offsets,
        total_scalar_words: next_word as usize,
        total_bindings,
    }
}

struct SpvEmitter {
    next_id: u32,
    header_words: Vec<u32>,
    deco_words: Vec<u32>,
    type_const_words: Vec<u32>,
    func_var_words: Vec<u32>,
    code_words: Vec<u32>,

    glsl_ext: u32,
    ty_void: u32,
    ty_bool: u32,
    ty_i32: u32,
    ty_u32: u32,
    ty_u64: u32,
    ty_f32: u32,
    ty_f64: u32,
    ty_ptr_fn_i32: u32,
    ty_ptr_fn_f32: u32,
    ty_ptr_fn_f64: u32,
    ty_ptr_fn_bool: u32,
    ty_ptr_unif_u32: u32,
    ty_ptr_unif_f32: u32,
    ty_ptr_unif_f64: u32,

    c_u0: u32,
    c_true: u32,
    c_false: u32,
    c_i32_cache: HashMap<i32, u32>,
    c_u32_cache: HashMap<u32, u32>,
    c_f32_cache: HashMap<u32, u32>,
    c_f64_cache: HashMap<u64, u32>,

    var_gid: u32,
    var_bid: u32,
    var_tid: u32,
    var_gdim: u32,
    local_size_x: u32,

    buf_vars: HashMap<usize, (u32, KernelScalarType)>,
    scalar_var: Option<u32>,
    scalar_offsets: HashMap<usize, u32>,
    local_vars: HashMap<String, (KernelScalarType, u32)>,
}

impl SpvEmitter {
    fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn emit(dst: &mut Vec<u32>, opcode: u16, operands: &[u32]) {
        let wc = (1 + operands.len()) as u32;
        dst.push((wc << 16) | u32::from(opcode));
        dst.extend_from_slice(operands);
    }

    fn push_str(dst: &mut Vec<u32>, s: &str) {
        let bytes = s.as_bytes();
        let n_words = (bytes.len() + 1).div_ceil(4);
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

    fn const_i32(&mut self, v: i32) -> u32 {
        if let Some(&id) = self.c_i32_cache.get(&v) {
            return id;
        }
        let id = self.alloc_id();
        Self::emit(
            &mut self.type_const_words,
            SPV_OP_CONSTANT,
            &[self.ty_i32, id, v as u32],
        );
        self.c_i32_cache.insert(v, id);
        id
    }

    fn const_u32(&mut self, v: u32) -> u32 {
        if let Some(&id) = self.c_u32_cache.get(&v) {
            return id;
        }
        let id = self.alloc_id();
        Self::emit(
            &mut self.type_const_words,
            SPV_OP_CONSTANT,
            &[self.ty_u32, id, v],
        );
        self.c_u32_cache.insert(v, id);
        id
    }

    fn const_f32(&mut self, v: f32) -> u32 {
        let bits = v.to_bits();
        if let Some(&id) = self.c_f32_cache.get(&bits) {
            return id;
        }
        let id = self.alloc_id();
        Self::emit(
            &mut self.type_const_words,
            SPV_OP_CONSTANT,
            &[self.ty_f32, id, bits],
        );
        self.c_f32_cache.insert(bits, id);
        id
    }

    fn const_f64(&mut self, v: f64) -> u32 {
        let bits = v.to_bits();
        if let Some(&id) = self.c_f64_cache.get(&bits) {
            return id;
        }
        let id = self.alloc_id();
        let lo = (bits & 0xFFFF_FFFF) as u32;
        let hi = (bits >> 32) as u32;
        Self::emit(
            &mut self.type_const_words,
            SPV_OP_CONSTANT,
            &[self.ty_f64, id, lo, hi],
        );
        self.c_f64_cache.insert(bits, id);
        id
    }

    fn spv_ty(&self, ty: KernelScalarType) -> u32 {
        match ty {
            KernelScalarType::I32 => self.ty_i32,
            KernelScalarType::F32 => self.ty_f32,
            KernelScalarType::F64 => self.ty_f64,
            KernelScalarType::Bool => self.ty_bool,
        }
    }

    fn spv_fn_ptr_ty(&self, ty: KernelScalarType) -> u32 {
        match ty {
            KernelScalarType::I32 => self.ty_ptr_fn_i32,
            KernelScalarType::F32 => self.ty_ptr_fn_f32,
            KernelScalarType::F64 => self.ty_ptr_fn_f64,
            KernelScalarType::Bool => self.ty_ptr_fn_bool,
        }
    }

    fn get_or_alloc_local(&mut self, name: &str, ty: KernelScalarType) -> u32 {
        if let Some(&(_, var_id)) = self.local_vars.get(name) {
            return var_id;
        }
        let var_id = self.alloc_id();
        let ptr_ty = self.spv_fn_ptr_ty(ty);
        Self::emit(
            &mut self.func_var_words,
            SPV_OP_VARIABLE,
            &[ptr_ty, var_id, SPV_SC_FUNCTION],
        );
        self.local_vars.insert(name.to_string(), (ty, var_id));
        var_id
    }

    fn emit_stmts(&mut self, stmts: &[KernelStmt]) {
        for s in stmts {
            match s {
                KernelStmt::Let { name, ty, expr } => {
                    let val = self.emit_expr(expr);
                    let var_id = self.get_or_alloc_local(name, *ty);
                    Self::emit(&mut self.code_words, SPV_OP_STORE, &[var_id, val]);
                }
                KernelStmt::Assign { name, expr } => {
                    let val = self.emit_expr(expr);
                    if let Some(&(_, var_id)) = self.local_vars.get(name) {
                        Self::emit(&mut self.code_words, SPV_OP_STORE, &[var_id, val]);
                    }
                }
                KernelStmt::Store {
                    buf_param,
                    index,
                    value,
                } => {
                    let idx_id = self.emit_expr(index);
                    let val_id = self.emit_expr(value);
                    let &(buf_var, elem_ty) =
                        self.buf_vars.get(buf_param).expect("buf param registered");
                    let ptr_ty = match elem_ty {
                        KernelScalarType::F64 => self.ty_ptr_unif_f64,
                        _ => self.ty_ptr_unif_f32,
                    };
                    let ptr = self.alloc_id();
                    Self::emit(
                        &mut self.code_words,
                        SPV_OP_ACCESS_CHAIN,
                        &[ptr_ty, ptr, buf_var, self.c_u0, idx_id],
                    );
                    Self::emit(&mut self.code_words, SPV_OP_STORE, &[ptr, val_id]);
                }
                KernelStmt::If {
                    cond,
                    then_body,
                    else_body,
                } => {
                    let cond_id = self.emit_expr(cond);
                    let lbl_then = self.alloc_id();
                    let lbl_merge = self.alloc_id();
                    if else_body.is_empty() {
                        Self::emit(
                            &mut self.code_words,
                            SPV_OP_SELECTION_MERGE,
                            &[lbl_merge, 0],
                        );
                        Self::emit(
                            &mut self.code_words,
                            SPV_OP_BRANCH_CONDITIONAL,
                            &[cond_id, lbl_then, lbl_merge],
                        );
                        Self::emit(&mut self.code_words, SPV_OP_LABEL, &[lbl_then]);
                        self.emit_stmts(then_body);
                        Self::emit(&mut self.code_words, SPV_OP_BRANCH, &[lbl_merge]);
                        Self::emit(&mut self.code_words, SPV_OP_LABEL, &[lbl_merge]);
                    } else {
                        let lbl_else = self.alloc_id();
                        Self::emit(
                            &mut self.code_words,
                            SPV_OP_SELECTION_MERGE,
                            &[lbl_merge, 0],
                        );
                        Self::emit(
                            &mut self.code_words,
                            SPV_OP_BRANCH_CONDITIONAL,
                            &[cond_id, lbl_then, lbl_else],
                        );
                        Self::emit(&mut self.code_words, SPV_OP_LABEL, &[lbl_then]);
                        self.emit_stmts(then_body);
                        Self::emit(&mut self.code_words, SPV_OP_BRANCH, &[lbl_merge]);
                        Self::emit(&mut self.code_words, SPV_OP_LABEL, &[lbl_else]);
                        self.emit_stmts(else_body);
                        Self::emit(&mut self.code_words, SPV_OP_BRANCH, &[lbl_merge]);
                        Self::emit(&mut self.code_words, SPV_OP_LABEL, &[lbl_merge]);
                    }
                }
                KernelStmt::While { cond, body } => {
                    let lbl_header = self.alloc_id();
                    let lbl_body = self.alloc_id();
                    let lbl_cont = self.alloc_id();
                    let lbl_merge = self.alloc_id();
                    Self::emit(&mut self.code_words, SPV_OP_BRANCH, &[lbl_header]);
                    Self::emit(&mut self.code_words, SPV_OP_LABEL, &[lbl_header]);
                    Self::emit(
                        &mut self.code_words,
                        SPV_OP_LOOP_MERGE,
                        &[lbl_merge, lbl_cont, 0],
                    );
                    let cond_id = self.emit_expr(cond);
                    Self::emit(
                        &mut self.code_words,
                        SPV_OP_BRANCH_CONDITIONAL,
                        &[cond_id, lbl_body, lbl_merge],
                    );
                    Self::emit(&mut self.code_words, SPV_OP_LABEL, &[lbl_body]);
                    self.emit_stmts(body);
                    Self::emit(&mut self.code_words, SPV_OP_BRANCH, &[lbl_cont]);
                    Self::emit(&mut self.code_words, SPV_OP_LABEL, &[lbl_cont]);
                    Self::emit(&mut self.code_words, SPV_OP_BRANCH, &[lbl_header]);
                    Self::emit(&mut self.code_words, SPV_OP_LABEL, &[lbl_merge]);
                }
            }
        }
    }

    fn load_scalar_u32(&mut self, word_off: u32) -> u32 {
        let svar = self.scalar_var.expect("scalar buffer present");
        let idx = self.const_u32(word_off);
        let ptr = self.alloc_id();
        Self::emit(
            &mut self.code_words,
            SPV_OP_ACCESS_CHAIN,
            &[self.ty_ptr_unif_u32, ptr, svar, self.c_u0, idx],
        );
        let val = self.alloc_id();
        Self::emit(&mut self.code_words, SPV_OP_LOAD, &[self.ty_u32, val, ptr]);
        val
    }

    fn load_builtin_x_i32(&mut self, var_vec3: u32, ty_v3u32: u32) -> u32 {
        let vec_val = self.alloc_id();
        Self::emit(
            &mut self.code_words,
            SPV_OP_LOAD,
            &[ty_v3u32, vec_val, var_vec3],
        );
        let ux = self.alloc_id();
        Self::emit(
            &mut self.code_words,
            SPV_OP_COMPOSITE_EXTRACT,
            &[self.ty_u32, ux, vec_val, 0],
        );
        let ix = self.alloc_id();
        Self::emit(&mut self.code_words, SPV_OP_BITCAST, &[self.ty_i32, ix, ux]);
        ix
    }

    fn emit_expr(&mut self, expr: &KernelExpr) -> u32 {
        match expr {
            KernelExpr::ConstI32(v) => self.const_i32(*v),
            KernelExpr::ConstF32(v) => self.const_f32(*v),
            KernelExpr::ConstF64(v) => self.const_f64(*v),
            KernelExpr::ConstBool(true) => self.c_true,
            KernelExpr::ConstBool(false) => self.c_false,
            KernelExpr::Param(idx, ty) => {
                let off = self.scalar_offsets[idx];
                match ty {
                    KernelScalarType::I32 => {
                        let w = self.load_scalar_u32(off);
                        let res = self.alloc_id();
                        Self::emit(&mut self.code_words, SPV_OP_BITCAST, &[self.ty_i32, res, w]);
                        res
                    }
                    KernelScalarType::F32 => {
                        let w = self.load_scalar_u32(off);
                        let res = self.alloc_id();
                        Self::emit(&mut self.code_words, SPV_OP_BITCAST, &[self.ty_f32, res, w]);
                        res
                    }
                    KernelScalarType::F64 => {
                        let lo = self.load_scalar_u32(off);
                        let hi = self.load_scalar_u32(off + 1);
                        let lo64 = self.alloc_id();
                        let hi64 = self.alloc_id();
                        let shift32 = self.const_u32(32);
                        let hi_sh = self.alloc_id();
                        let bits = self.alloc_id();
                        let res = self.alloc_id();
                        Self::emit(
                            &mut self.code_words,
                            SPV_OP_UCONVERT,
                            &[self.ty_u64, lo64, lo],
                        );
                        Self::emit(
                            &mut self.code_words,
                            SPV_OP_UCONVERT,
                            &[self.ty_u64, hi64, hi],
                        );
                        Self::emit(
                            &mut self.code_words,
                            SPV_OP_SHIFT_LEFT_LOGICAL,
                            &[self.ty_u64, hi_sh, hi64, shift32],
                        );
                        Self::emit(
                            &mut self.code_words,
                            SPV_OP_BITWISE_OR,
                            &[self.ty_u64, bits, lo64, hi_sh],
                        );
                        Self::emit(
                            &mut self.code_words,
                            SPV_OP_BITCAST,
                            &[self.ty_f64, res, bits],
                        );
                        res
                    }
                    KernelScalarType::Bool => self.c_false,
                }
            }
            KernelExpr::Var(name, ty) => {
                let &(_, var_id) = self.local_vars.get(name).expect("local declared");
                let res = self.alloc_id();
                let sty = self.spv_ty(*ty);
                Self::emit(&mut self.code_words, SPV_OP_LOAD, &[sty, res, var_id]);
                res
            }
            KernelExpr::GlobalId => {
                let ty_v3 = self.var_gid_vec_ty();
                self.load_builtin_x_i32(self.var_gid, ty_v3)
            }
            KernelExpr::BlockIdx => {
                let ty_v3 = self.var_gid_vec_ty();
                self.load_builtin_x_i32(self.var_bid, ty_v3)
            }
            KernelExpr::ThreadIdx => {
                let ty_v3 = self.var_gid_vec_ty();
                self.load_builtin_x_i32(self.var_tid, ty_v3)
            }
            KernelExpr::GridDim => {
                let ty_v3 = self.var_gid_vec_ty();
                self.load_builtin_x_i32(self.var_gdim, ty_v3)
            }
            KernelExpr::BlockDim => self.const_i32(self.local_size_x as i32),
            KernelExpr::Load {
                buf_param,
                index,
                elem_ty,
            } => {
                let idx_id = self.emit_expr(index);
                let &(buf_var, _) = self.buf_vars.get(buf_param).expect("buf registered");
                let (ptr_ty, val_ty) = match elem_ty {
                    KernelScalarType::F64 => (self.ty_ptr_unif_f64, self.ty_f64),
                    _ => (self.ty_ptr_unif_f32, self.ty_f32),
                };
                let ptr = self.alloc_id();
                Self::emit(
                    &mut self.code_words,
                    SPV_OP_ACCESS_CHAIN,
                    &[ptr_ty, ptr, buf_var, self.c_u0, idx_id],
                );
                let val = self.alloc_id();
                Self::emit(&mut self.code_words, SPV_OP_LOAD, &[val_ty, val, ptr]);
                val
            }
            KernelExpr::Unary { op, arg, ty } => {
                let src = self.emit_expr(arg);
                let sty = self.spv_ty(*ty);
                let dst = self.alloc_id();
                match op {
                    KernelUnaryOp::Neg => {
                        let opcode = if *ty == KernelScalarType::I32 {
                            SPV_OP_SNEGATE
                        } else {
                            SPV_OP_FNEGATE
                        };
                        Self::emit(&mut self.code_words, opcode, &[sty, dst, src]);
                    }
                    KernelUnaryOp::Not => {
                        Self::emit(&mut self.code_words, SPV_OP_LOGICAL_NOT, &[sty, dst, src]);
                    }
                    KernelUnaryOp::Abs => {
                        let inst = if *ty == KernelScalarType::I32 {
                            GLSL_SABS
                        } else {
                            GLSL_FABS
                        };
                        Self::emit(
                            &mut self.code_words,
                            SPV_OP_EXT_INST,
                            &[sty, dst, self.glsl_ext, inst, src],
                        );
                    }
                    KernelUnaryOp::Exp => {
                        // GLSL.std.450 Exp/Tanh accept 32-bit float; convert f64 cleanly if needed.
                        if *ty == KernelScalarType::F64 {
                            let s32 = self.alloc_id();
                            let e32 = self.alloc_id();
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_FCONVERT,
                                &[self.ty_f32, s32, src],
                            );
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_EXT_INST,
                                &[self.ty_f32, e32, self.glsl_ext, GLSL_EXP, s32],
                            );
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_FCONVERT,
                                &[self.ty_f64, dst, e32],
                            );
                        } else {
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_EXT_INST,
                                &[sty, dst, self.glsl_ext, GLSL_EXP, src],
                            );
                        }
                    }
                    KernelUnaryOp::Sqrt => {
                        Self::emit(
                            &mut self.code_words,
                            SPV_OP_EXT_INST,
                            &[sty, dst, self.glsl_ext, GLSL_SQRT, src],
                        );
                    }
                    KernelUnaryOp::Tanh => {
                        if *ty == KernelScalarType::F64 {
                            let s32 = self.alloc_id();
                            let t32 = self.alloc_id();
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_FCONVERT,
                                &[self.ty_f32, s32, src],
                            );
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_EXT_INST,
                                &[self.ty_f32, t32, self.glsl_ext, GLSL_TANH, s32],
                            );
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_FCONVERT,
                                &[self.ty_f64, dst, t32],
                            );
                        } else {
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_EXT_INST,
                                &[sty, dst, self.glsl_ext, GLSL_TANH, src],
                            );
                        }
                    }
                    KernelUnaryOp::ToF32 => match arg.ty() {
                        KernelScalarType::I32 => {
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_CONVERT_S_TO_F,
                                &[self.ty_f32, dst, src],
                            );
                        }
                        KernelScalarType::F64 => {
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_FCONVERT,
                                &[self.ty_f32, dst, src],
                            );
                        }
                        _ => return src,
                    },
                    KernelUnaryOp::ToF64 => match arg.ty() {
                        KernelScalarType::I32 => {
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_CONVERT_S_TO_F,
                                &[self.ty_f64, dst, src],
                            );
                        }
                        KernelScalarType::F32 => {
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_FCONVERT,
                                &[self.ty_f64, dst, src],
                            );
                        }
                        _ => return src,
                    },
                    KernelUnaryOp::ToI32 => match arg.ty() {
                        KernelScalarType::F32 | KernelScalarType::F64 => {
                            Self::emit(
                                &mut self.code_words,
                                SPV_OP_CONVERT_F_TO_S,
                                &[self.ty_i32, dst, src],
                            );
                        }
                        _ => return src,
                    },
                }
                dst
            }
            KernelExpr::Binary { op, lhs, rhs, ty } => {
                let l = self.emit_expr(lhs);
                let r = self.emit_expr(rhs);
                let lty = lhs.ty();
                let sty = self.spv_ty(*ty);
                let dst = self.alloc_id();
                match op {
                    KernelBinOp::Min | KernelBinOp::Max => {
                        let inst = match (op, lty) {
                            (KernelBinOp::Min, KernelScalarType::I32) => GLSL_SMIN,
                            (KernelBinOp::Max, KernelScalarType::I32) => GLSL_SMAX,
                            (KernelBinOp::Min, _) => GLSL_FMIN,
                            (KernelBinOp::Max, _) => GLSL_FMAX,
                            _ => unreachable!(),
                        };
                        Self::emit(
                            &mut self.code_words,
                            SPV_OP_EXT_INST,
                            &[sty, dst, self.glsl_ext, inst, l, r],
                        );
                    }
                    _ => {
                        let opcode = match (op, lty) {
                            (KernelBinOp::Add, KernelScalarType::I32) => SPV_OP_IADD,
                            (KernelBinOp::Add, _) => SPV_OP_FADD,
                            (KernelBinOp::Sub, KernelScalarType::I32) => SPV_OP_ISUB,
                            (KernelBinOp::Sub, _) => SPV_OP_FSUB,
                            (KernelBinOp::Mul, KernelScalarType::I32) => SPV_OP_IMUL,
                            (KernelBinOp::Mul, _) => SPV_OP_FMUL,
                            (KernelBinOp::Div, KernelScalarType::I32) => SPV_OP_SDIV,
                            (KernelBinOp::Div, _) => SPV_OP_FDIV,
                            (KernelBinOp::Rem, KernelScalarType::I32) => SPV_OP_SREM,
                            (KernelBinOp::Lt, KernelScalarType::I32) => SPV_OP_SLESS_THAN,
                            (KernelBinOp::Lt, _) => SPV_OP_FORD_LESS_THAN,
                            (KernelBinOp::Le, KernelScalarType::I32) => SPV_OP_SLESS_THAN_EQUAL,
                            (KernelBinOp::Le, _) => SPV_OP_FORD_LESS_THAN_EQUAL,
                            (KernelBinOp::Gt, KernelScalarType::I32) => SPV_OP_SGREATER_THAN,
                            (KernelBinOp::Gt, _) => SPV_OP_FORD_GREATER_THAN,
                            (KernelBinOp::Ge, KernelScalarType::I32) => SPV_OP_SGREATER_THAN_EQUAL,
                            (KernelBinOp::Ge, _) => SPV_OP_FORD_GREATER_THAN_EQUAL,
                            (KernelBinOp::Eq, KernelScalarType::I32) => SPV_OP_IEQUAL,
                            (KernelBinOp::Eq, KernelScalarType::Bool) => SPV_OP_LOGICAL_EQUAL,
                            (KernelBinOp::Eq, _) => SPV_OP_FORD_EQUAL,
                            (KernelBinOp::Ne, KernelScalarType::I32) => SPV_OP_INOT_EQUAL,
                            (KernelBinOp::Ne, KernelScalarType::Bool) => SPV_OP_LOGICAL_NOT_EQUAL,
                            (KernelBinOp::Ne, _) => SPV_OP_FORD_NOT_EQUAL,
                            (KernelBinOp::And, _) => SPV_OP_LOGICAL_AND,
                            (KernelBinOp::Or, _) => SPV_OP_LOGICAL_OR,
                            _ => SPV_OP_IADD,
                        };
                        Self::emit(&mut self.code_words, opcode, &[sty, dst, l, r]);
                    }
                }
                dst
            }
        }
    }

    fn var_gid_vec_ty(&self) -> u32 {
        // Stored in header_words[0] sentinel? No, we keep ty_v3u32 right after ty_f64.
        self.ty_f64 + 1
    }
}

/// Compile a typed [`KernelProgram`] into valid SPIR-V 1.0 `GLCompute` words.
pub(crate) fn emit_spirv_words(
    program: &KernelProgram,
    local_size_x: u32,
) -> Result<Vec<u32>, String> {
    let local_size_x = local_size_x.max(1);
    let uses_f64 = program.uses_f64();
    let layout = vulkan_kernel_layout(program);

    let mut em = SpvEmitter {
        next_id: 1,
        header_words: Vec::new(),
        deco_words: Vec::new(),
        type_const_words: Vec::new(),
        func_var_words: Vec::new(),
        code_words: Vec::new(),
        glsl_ext: 0,
        ty_void: 0,
        ty_bool: 0,
        ty_i32: 0,
        ty_u32: 0,
        ty_u64: 0,
        ty_f32: 0,
        ty_f64: 0,
        ty_ptr_fn_i32: 0,
        ty_ptr_fn_f32: 0,
        ty_ptr_fn_f64: 0,
        ty_ptr_fn_bool: 0,
        ty_ptr_unif_u32: 0,
        ty_ptr_unif_f32: 0,
        ty_ptr_unif_f64: 0,
        c_u0: 0,
        c_true: 0,
        c_false: 0,
        c_i32_cache: HashMap::new(),
        c_u32_cache: HashMap::new(),
        c_f32_cache: HashMap::new(),
        c_f64_cache: HashMap::new(),
        var_gid: 0,
        var_bid: 0,
        var_tid: 0,
        var_gdim: 0,
        local_size_x,
        buf_vars: HashMap::new(),
        scalar_var: None,
        scalar_offsets: layout.scalar_word_offsets.clone(),
        local_vars: HashMap::new(),
    };

    // Capabilities & GLSL.std.450 import
    SpvEmitter::emit(&mut em.header_words, SPV_OP_CAPABILITY, &[1]); // Shader
    if uses_f64 {
        SpvEmitter::emit(&mut em.header_words, SPV_OP_CAPABILITY, &[10]); // Float64
        SpvEmitter::emit(&mut em.header_words, SPV_OP_CAPABILITY, &[11]); // Int64
    }
    em.glsl_ext = em.alloc_id();
    let mut ext_ops = vec![em.glsl_ext];
    SpvEmitter::push_str(&mut ext_ops, "GLSL.std.450");
    SpvEmitter::emit(&mut em.header_words, SPV_OP_EXT_INST_IMPORT, &ext_ops);
    SpvEmitter::emit(&mut em.header_words, SPV_OP_MEMORY_MODEL, &[0, 1]); // Logical, GLSL450

    // Core types (keep ty_v3u32 immediately after ty_f64 so var_gid_vec_ty() == ty_f64 + 1!)
    em.ty_void = em.alloc_id();
    em.ty_bool = em.alloc_id();
    em.ty_i32 = em.alloc_id();
    em.ty_u32 = em.alloc_id();
    em.ty_u64 = em.alloc_id();
    em.ty_f32 = em.alloc_id();
    em.ty_f64 = em.alloc_id();
    let ty_v3u32 = em.alloc_id();
    debug_assert_eq!(ty_v3u32, em.ty_f64 + 1);
    let ty_fn = em.alloc_id();
    let ty_ptr_in_v3u32 = em.alloc_id();
    em.ty_ptr_fn_i32 = em.alloc_id();
    em.ty_ptr_fn_f32 = em.alloc_id();
    em.ty_ptr_fn_f64 = em.alloc_id();
    em.ty_ptr_fn_bool = em.alloc_id();

    SpvEmitter::emit(&mut em.type_const_words, SPV_OP_TYPE_VOID, &[em.ty_void]);
    SpvEmitter::emit(&mut em.type_const_words, SPV_OP_TYPE_BOOL, &[em.ty_bool]);
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_INT,
        &[em.ty_i32, 32, 1],
    );
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_INT,
        &[em.ty_u32, 32, 0],
    );
    if uses_f64 {
        SpvEmitter::emit(
            &mut em.type_const_words,
            SPV_OP_TYPE_INT,
            &[em.ty_u64, 64, 0],
        );
    }
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_FLOAT,
        &[em.ty_f32, 32],
    );
    if uses_f64 {
        SpvEmitter::emit(
            &mut em.type_const_words,
            SPV_OP_TYPE_FLOAT,
            &[em.ty_f64, 64],
        );
    }
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_VECTOR,
        &[ty_v3u32, em.ty_u32, 3],
    );
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_FUNCTION,
        &[ty_fn, em.ty_void],
    );
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_POINTER,
        &[ty_ptr_in_v3u32, SPV_SC_INPUT, ty_v3u32],
    );
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_POINTER,
        &[em.ty_ptr_fn_i32, SPV_SC_FUNCTION, em.ty_i32],
    );
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_POINTER,
        &[em.ty_ptr_fn_f32, SPV_SC_FUNCTION, em.ty_f32],
    );
    if uses_f64 {
        SpvEmitter::emit(
            &mut em.type_const_words,
            SPV_OP_TYPE_POINTER,
            &[em.ty_ptr_fn_f64, SPV_SC_FUNCTION, em.ty_f64],
        );
    }
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_POINTER,
        &[em.ty_ptr_fn_bool, SPV_SC_FUNCTION, em.ty_bool],
    );

    // Storage buffer runtime array + struct types
    let ty_arr_u32 = em.alloc_id();
    let ty_struct_u32 = em.alloc_id();
    let ty_ptr_struct_u32 = em.alloc_id();
    em.ty_ptr_unif_u32 = em.alloc_id();
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_RUNTIME_ARRAY,
        &[ty_arr_u32, em.ty_u32],
    );
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_STRUCT,
        &[ty_struct_u32, ty_arr_u32],
    );
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_POINTER,
        &[ty_ptr_struct_u32, SPV_SC_UNIFORM, ty_struct_u32],
    );
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_POINTER,
        &[em.ty_ptr_unif_u32, SPV_SC_UNIFORM, em.ty_u32],
    );
    SpvEmitter::emit(
        &mut em.deco_words,
        SPV_OP_DECORATE,
        &[ty_arr_u32, SPV_DEC_ARRAY_STRIDE, 4],
    );
    SpvEmitter::emit(
        &mut em.deco_words,
        SPV_OP_DECORATE,
        &[ty_struct_u32, SPV_DEC_BUFFER_BLOCK],
    );
    SpvEmitter::emit(
        &mut em.deco_words,
        SPV_OP_MEMBER_DECORATE,
        &[ty_struct_u32, 0, SPV_DEC_OFFSET, 0],
    );

    let ty_arr_f32 = em.alloc_id();
    let ty_struct_f32 = em.alloc_id();
    let ty_ptr_struct_f32 = em.alloc_id();
    em.ty_ptr_unif_f32 = em.alloc_id();
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_RUNTIME_ARRAY,
        &[ty_arr_f32, em.ty_f32],
    );
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_STRUCT,
        &[ty_struct_f32, ty_arr_f32],
    );
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_POINTER,
        &[ty_ptr_struct_f32, SPV_SC_UNIFORM, ty_struct_f32],
    );
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_TYPE_POINTER,
        &[em.ty_ptr_unif_f32, SPV_SC_UNIFORM, em.ty_f32],
    );
    SpvEmitter::emit(
        &mut em.deco_words,
        SPV_OP_DECORATE,
        &[ty_arr_f32, SPV_DEC_ARRAY_STRIDE, 4],
    );
    SpvEmitter::emit(
        &mut em.deco_words,
        SPV_OP_DECORATE,
        &[ty_struct_f32, SPV_DEC_BUFFER_BLOCK],
    );
    SpvEmitter::emit(
        &mut em.deco_words,
        SPV_OP_MEMBER_DECORATE,
        &[ty_struct_f32, 0, SPV_DEC_OFFSET, 0],
    );

    let ty_ptr_struct_f64 = if uses_f64 {
        let ty_arr_f64 = em.alloc_id();
        let ty_struct_f64 = em.alloc_id();
        let ty_ptr = em.alloc_id();
        em.ty_ptr_unif_f64 = em.alloc_id();
        SpvEmitter::emit(
            &mut em.type_const_words,
            SPV_OP_TYPE_RUNTIME_ARRAY,
            &[ty_arr_f64, em.ty_f64],
        );
        SpvEmitter::emit(
            &mut em.type_const_words,
            SPV_OP_TYPE_STRUCT,
            &[ty_struct_f64, ty_arr_f64],
        );
        SpvEmitter::emit(
            &mut em.type_const_words,
            SPV_OP_TYPE_POINTER,
            &[ty_ptr, SPV_SC_UNIFORM, ty_struct_f64],
        );
        SpvEmitter::emit(
            &mut em.type_const_words,
            SPV_OP_TYPE_POINTER,
            &[em.ty_ptr_unif_f64, SPV_SC_UNIFORM, em.ty_f64],
        );
        SpvEmitter::emit(
            &mut em.deco_words,
            SPV_OP_DECORATE,
            &[ty_arr_f64, SPV_DEC_ARRAY_STRIDE, 8],
        );
        SpvEmitter::emit(
            &mut em.deco_words,
            SPV_OP_DECORATE,
            &[ty_struct_f64, SPV_DEC_BUFFER_BLOCK],
        );
        SpvEmitter::emit(
            &mut em.deco_words,
            SPV_OP_MEMBER_DECORATE,
            &[ty_struct_f64, 0, SPV_DEC_OFFSET, 0],
        );
        ty_ptr
    } else {
        0
    };

    // Built-in constants
    em.c_u0 = em.const_u32(0);
    em.c_true = em.alloc_id();
    em.c_false = em.alloc_id();
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_CONSTANT_TRUE,
        &[em.ty_bool, em.c_true],
    );
    SpvEmitter::emit(
        &mut em.type_const_words,
        SPV_OP_CONSTANT_FALSE,
        &[em.ty_bool, em.c_false],
    );

    // Built-in input variables: GlobalInvocationId (28), WorkgroupId (26), LocalInvocationId (27), NumWorkgroups (24)
    em.var_gid = em.alloc_id();
    em.var_bid = em.alloc_id();
    em.var_tid = em.alloc_id();
    em.var_gdim = em.alloc_id();
    for (var_id, builtin_code) in [
        (em.var_gid, 28u32),
        (em.var_bid, 26u32),
        (em.var_tid, 27u32),
        (em.var_gdim, 24u32),
    ] {
        SpvEmitter::emit(
            &mut em.type_const_words,
            SPV_OP_VARIABLE,
            &[ty_ptr_in_v3u32, var_id, SPV_SC_INPUT],
        );
        SpvEmitter::emit(
            &mut em.deco_words,
            SPV_OP_DECORATE,
            &[var_id, SPV_DEC_BUILT_IN, builtin_code],
        );
    }

    // Storage buffer variables for every buffer parameter
    for &(param_idx, binding) in &layout.buf_bindings {
        let var_id = em.alloc_id();
        let (ptr_ty, elem_ty) = match program.params[param_idx].ty {
            KernelParamType::BufF64 => (ty_ptr_struct_f64, KernelScalarType::F64),
            _ => (ty_ptr_struct_f32, KernelScalarType::F32),
        };
        SpvEmitter::emit(
            &mut em.type_const_words,
            SPV_OP_VARIABLE,
            &[ptr_ty, var_id, SPV_SC_UNIFORM],
        );
        SpvEmitter::emit(
            &mut em.deco_words,
            SPV_OP_DECORATE,
            &[var_id, SPV_DEC_DESCRIPTOR_SET, 0],
        );
        SpvEmitter::emit(
            &mut em.deco_words,
            SPV_OP_DECORATE,
            &[var_id, SPV_DEC_BINDING, binding],
        );
        em.buf_vars.insert(param_idx, (var_id, elem_ty));
    }

    // Optional trailing scalar parameter buffer at binding = buf_bindings.len()
    if layout.total_scalar_words > 0 {
        let var_id = em.alloc_id();
        let binding = layout.buf_bindings.len() as u32;
        SpvEmitter::emit(
            &mut em.type_const_words,
            SPV_OP_VARIABLE,
            &[ty_ptr_struct_u32, var_id, SPV_SC_UNIFORM],
        );
        SpvEmitter::emit(
            &mut em.deco_words,
            SPV_OP_DECORATE,
            &[var_id, SPV_DEC_DESCRIPTOR_SET, 0],
        );
        SpvEmitter::emit(
            &mut em.deco_words,
            SPV_OP_DECORATE,
            &[var_id, SPV_DEC_BINDING, binding],
        );
        em.scalar_var = Some(var_id);
    }

    let fn_main = em.alloc_id();
    let entry_label = em.alloc_id();

    // EntryPoint & ExecutionMode
    let mut ep_ops = vec![5u32, fn_main]; // GLCompute = 5
    SpvEmitter::push_str(&mut ep_ops, "main");
    ep_ops.extend_from_slice(&[em.var_gid, em.var_bid, em.var_tid, em.var_gdim]);
    SpvEmitter::emit(&mut em.header_words, SPV_OP_ENTRY_POINT, &ep_ops);
    SpvEmitter::emit(
        &mut em.header_words,
        SPV_OP_EXECUTION_MODE,
        &[fn_main, 17, local_size_x, 1, 1], // LocalSize = 17
    );

    // Emit structured function body
    em.emit_stmts(&program.body);
    SpvEmitter::emit(&mut em.code_words, SPV_OP_RETURN, &[]);

    // Assemble final SPIR-V binary word stream in exact section order:
    // Header(5 words) -> Capabilities/Ext/MemModel/EntryPoint/ExecMode -> Decorations -> Types/Consts/GlobalVars -> Function
    let mut words = vec![
        0x0723_0203u32,
        0x0001_0000u32,
        0x0000_0000u32,
        em.next_id,
        0x0000_0000u32,
    ];
    words.extend_from_slice(&em.header_words);
    words.extend_from_slice(&em.deco_words);
    words.extend_from_slice(&em.type_const_words);
    SpvEmitter::emit(
        &mut words,
        SPV_OP_FUNCTION,
        &[em.ty_void, fn_main, 0, ty_fn],
    );
    SpvEmitter::emit(&mut words, SPV_OP_LABEL, &[entry_label]);
    words.extend_from_slice(&em.func_var_words);
    words.extend_from_slice(&em.code_words);
    SpvEmitter::emit(&mut words, SPV_OP_FUNCTION_END, &[]);
    Ok(words)
}

// ============================================================================
// 4. Backend 3: Metal Shading Language (MSL) Emitter
// ============================================================================

/// Compile a typed [`KernelProgram`] to Metal Shading Language (MSL) compute source.
pub(crate) fn emit_msl(program: &KernelProgram) -> String {
    let mut out = String::with_capacity(1024);
    let _ = writeln!(out, "#include <metal_stdlib>");
    let _ = writeln!(out, "using namespace metal;");
    let _ = writeln!(out);

    let mut scalar_fields = Vec::new();
    for (idx, p) in program.params.iter().enumerate() {
        match p.ty {
            KernelParamType::F32 => scalar_fields.push((idx, "float", p.name.as_str())),
            KernelParamType::F64 => scalar_fields.push((idx, "float", p.name.as_str())),
            KernelParamType::I32 => scalar_fields.push((idx, "int", p.name.as_str())),
            KernelParamType::BufF32 | KernelParamType::BufF64 => {}
        }
    }
    if !scalar_fields.is_empty() {
        let _ = writeln!(out, "struct _MoraKernelParams {{");
        for (_, mty, name) in &scalar_fields {
            let _ = writeln!(out, "    {mty} {name};");
        }
        let _ = writeln!(out, "}};");
        let _ = writeln!(out);
    }

    let mut sig = Vec::new();
    let mut buf_slot = 0u32;
    for p in &program.params {
        match p.ty {
            KernelParamType::BufF32 => {
                sig.push(format!("device float* {} [[buffer({buf_slot})]]", p.name));
                buf_slot += 1;
            }
            KernelParamType::BufF64 => {
                // Metal does not support 64-bit float in MSL; lower cleanly to float buffers.
                sig.push(format!("device float* {} [[buffer({buf_slot})]]", p.name));
                buf_slot += 1;
            }
            _ => {}
        }
    }
    if !scalar_fields.is_empty() {
        sig.push(format!(
            "constant _MoraKernelParams& _kp [[buffer({buf_slot})]]"
        ));
    }
    sig.push("uint3 _gid [[thread_position_in_grid]]".to_string());
    sig.push("uint3 _bid [[threadgroup_position_in_grid]]".to_string());
    sig.push("uint3 _tid [[thread_position_in_threadgroup]]".to_string());
    sig.push("uint3 _bdim [[threads_per_threadgroup]]".to_string());
    sig.push("uint3 _gdim [[threadgroups_per_grid]]".to_string());

    let _ = writeln!(out, "kernel void {}(", program.name);
    let _ = writeln!(out, "    {}", sig.join(",\n    "));
    let _ = writeln!(out, ") {{");
    emit_msl_stmts(&mut out, program, &program.body, 1);
    let _ = writeln!(out, "}}");
    out
}

fn emit_msl_stmts(out: &mut String, program: &KernelProgram, stmts: &[KernelStmt], indent: usize) {
    let pad = "    ".repeat(indent);
    for s in stmts {
        match s {
            KernelStmt::Let { name, ty, expr } => {
                let mty = match ty {
                    KernelScalarType::I32 => "int",
                    KernelScalarType::F32 | KernelScalarType::F64 => "float",
                    KernelScalarType::Bool => "bool",
                };
                let e = emit_msl_expr(program, expr);
                let _ = writeln!(out, "{pad}{mty} {name} = {e};");
            }
            KernelStmt::Assign { name, expr } => {
                let e = emit_msl_expr(program, expr);
                let _ = writeln!(out, "{pad}{name} = {e};");
            }
            KernelStmt::Store {
                buf_param,
                index,
                value,
            } => {
                let bname = &program.params[*buf_param].name;
                let idx = emit_msl_expr(program, index);
                let val = emit_msl_expr(program, value);
                let _ = writeln!(out, "{pad}{bname}[{idx}] = {val};");
            }
            KernelStmt::If {
                cond,
                then_body,
                else_body,
            } => {
                let c = emit_msl_expr(program, cond);
                let _ = writeln!(out, "{pad}if ({c}) {{");
                emit_msl_stmts(out, program, then_body, indent + 1);
                if else_body.is_empty() {
                    let _ = writeln!(out, "{pad}}}");
                } else {
                    let _ = writeln!(out, "{pad}}} else {{");
                    emit_msl_stmts(out, program, else_body, indent + 1);
                    let _ = writeln!(out, "{pad}}}");
                }
            }
            KernelStmt::While { cond, body } => {
                let c = emit_msl_expr(program, cond);
                let _ = writeln!(out, "{pad}while ({c}) {{");
                emit_msl_stmts(out, program, body, indent + 1);
                let _ = writeln!(out, "{pad}}}");
            }
        }
    }
}

fn emit_msl_expr(program: &KernelProgram, expr: &KernelExpr) -> String {
    match expr {
        KernelExpr::ConstI32(v) => format!("{v}"),
        KernelExpr::ConstF32(v) => format!("{v:?}f"),
        KernelExpr::ConstF64(v) => format!("{:?}f", *v as f32),
        KernelExpr::ConstBool(b) => if *b { "true" } else { "false" }.to_string(),
        KernelExpr::Param(idx, _) => format!("_kp.{}", program.params[*idx].name),
        KernelExpr::Var(name, _) => name.clone(),
        KernelExpr::GlobalId => "int(_gid.x)".to_string(),
        KernelExpr::BlockIdx => "int(_bid.x)".to_string(),
        KernelExpr::ThreadIdx => "int(_tid.x)".to_string(),
        KernelExpr::BlockDim => "int(_bdim.x)".to_string(),
        KernelExpr::GridDim => "int(_gdim.x)".to_string(),
        KernelExpr::Load {
            buf_param, index, ..
        } => {
            format!(
                "{}[{}]",
                program.params[*buf_param].name,
                emit_msl_expr(program, index)
            )
        }
        KernelExpr::Unary { op, arg, .. } => {
            let a = emit_msl_expr(program, arg);
            match op {
                KernelUnaryOp::Neg => format!("(-({a}))"),
                KernelUnaryOp::Not => format!("(!({a}))"),
                KernelUnaryOp::Exp => format!("exp({a})"),
                KernelUnaryOp::Sqrt => format!("sqrt({a})"),
                KernelUnaryOp::Tanh => format!("tanh({a})"),
                KernelUnaryOp::Abs => format!("abs({a})"),
                KernelUnaryOp::ToF32 | KernelUnaryOp::ToF64 => format!("float({a})"),
                KernelUnaryOp::ToI32 => format!("int({a})"),
            }
        }
        KernelExpr::Binary { op, lhs, rhs, .. } => {
            let l = emit_msl_expr(program, lhs);
            let r = emit_msl_expr(program, rhs);
            match op {
                KernelBinOp::Min => format!("min({l}, {r})"),
                KernelBinOp::Max => format!("max({l}, {r})"),
                _ => {
                    let sym = match op {
                        KernelBinOp::Add => "+",
                        KernelBinOp::Sub => "-",
                        KernelBinOp::Mul => "*",
                        KernelBinOp::Div => "/",
                        KernelBinOp::Rem => "%",
                        KernelBinOp::Lt => "<",
                        KernelBinOp::Le => "<=",
                        KernelBinOp::Gt => ">",
                        KernelBinOp::Ge => ">=",
                        KernelBinOp::Eq => "==",
                        KernelBinOp::Ne => "!=",
                        KernelBinOp::And => "&&",
                        KernelBinOp::Or => "||",
                        KernelBinOp::Min | KernelBinOp::Max => unreachable!(),
                    };
                    format!("({l} {sym} {r})")
                }
            }
        }
    }
}

// ============================================================================
// 5. Backend 4: Safe CPU Executor & Unified Host/GPU Dispatcher
// ============================================================================

#[derive(Clone, Copy)]
enum CpuScalar {
    I32(i32),
    F32(f32),
    F64(f64),
    Bool(bool),
}

enum CpuParamBuf {
    F32(Vec<f32>),
    F64(Vec<f64>),
    Scalar(CpuScalar),
}

/// Execute a [`CompiledGpuKernel`] on CPU or live GPU VRAM depending on buffer tensor placement.
pub(crate) fn launch_compiled_kernel(
    kernel: &CompiledGpuKernel,
    grid: u32,
    block: u32,
    args: &[crate::value::Value],
) -> MoraResult<()> {
    use crate::common::Device;
    use crate::value::Value;

    if grid == 0 || block == 0 {
        return Err(MoraError::Runtime(format!(
            "kernel '{}': grid ({grid}) and block ({block}) must be positive integers",
            kernel.name()
        )));
    }
    if args.len() != kernel.params().len() {
        return Err(MoraError::Runtime(format!(
            "kernel '{}' expects {} argument(s), got {}",
            kernel.name(),
            kernel.params().len(),
            args.len()
        )));
    }

    // 1. Validate parameter types & collect target execution device across all buffer tensors.
    let mut target_device: Option<Device> = None;
    let mut buf_tensors: Vec<(usize, Arc<crate::value::tensor::Tensor>)> = Vec::new();
    for (idx, (param, arg)) in kernel.params().iter().zip(args.iter()).enumerate() {
        match param.ty {
            KernelParamType::BufF32 | KernelParamType::BufF64 => {
                let expected_dt = param.ty.buffer_dtype().expect("buffer dtype");
                let Value::Tensor(t) = arg else {
                    return Err(MoraError::Runtime(format!(
                        "kernel '{}': parameter '{}' expects Tensor<{}>, got {}",
                        kernel.name(),
                        param.name,
                        expected_dt.canonical_type_name(),
                        crate::flow::type_name(arg)
                    )));
                };
                if t.dtype != expected_dt {
                    return Err(MoraError::Runtime(format!(
                        "kernel '{}': parameter '{}' expects Tensor<{}>, got Tensor<{}>",
                        kernel.name(),
                        param.name,
                        expected_dt.canonical_type_name(),
                        t.dtype.canonical_type_name()
                    )));
                }
                if !t.is_contiguous() || t.offset != 0 {
                    return Err(MoraError::Runtime(format!(
                        "kernel '{}': buffer parameter '{}' must be a contiguous zero-offset Tensor (call .contiguous())",
                        kernel.name(),
                        param.name
                    )));
                }
                if let Some(ref dev) = target_device {
                    if *dev != t.device {
                        return Err(MoraError::Runtime(format!(
                            "kernel '{}': cross-device launch forbidden ('{}' is on {} but earlier buffer is on {})",
                            kernel.name(),
                            param.name,
                            t.device,
                            dev
                        )));
                    }
                } else {
                    target_device = Some(t.device.clone());
                }
                buf_tensors.push((idx, Arc::clone(t)));
            }
            KernelParamType::F32 | KernelParamType::F64 => {
                if !matches!(arg, Value::Float(_) | Value::Int(_) | Value::Scalar(_)) {
                    return Err(MoraError::Runtime(format!(
                        "kernel '{}': scalar parameter '{}' expects numeric value, got {}",
                        kernel.name(),
                        param.name,
                        crate::flow::type_name(arg)
                    )));
                }
            }
            KernelParamType::I32 => {
                if !matches!(arg, Value::Int(_) | Value::Scalar(_)) {
                    return Err(MoraError::Runtime(format!(
                        "kernel '{}': integer parameter '{}' expects int/i32, got {}",
                        kernel.name(),
                        param.name,
                        crate::flow::type_name(arg)
                    )));
                }
            }
        }
    }

    let exec_device = target_device.unwrap_or(Device::Cpu);

    // 2. Hardware GPU fast path (zero H2D / D2H on persistent VRAM tensors!).
    if !matches!(exec_device, Device::Cpu)
        && let Some(backend) = crate::runtime::gpu::resolve_hardware_backend(&exec_device)
        && backend.supports_vram_residency()
    {
        let mut launch_args = Vec::with_capacity(kernel.params().len());
        let mut all_vram = true;
        for (param, arg) in kernel.params().iter().zip(args.iter()) {
            match param.ty {
                KernelParamType::BufF32 | KernelParamType::BufF64 => {
                    let Value::Tensor(t) = arg else {
                        unreachable!();
                    };
                    if let Some(vbuf) = t.vram_buffer_if_contiguous() {
                        launch_args.push(KernelLaunchArg::VramBuffer {
                            handle: vbuf.raw_handle(),
                            byte_len: vbuf.byte_len(),
                            dtype: t.dtype,
                        });
                    } else {
                        all_vram = false;
                        break;
                    }
                }
                KernelParamType::F32 => {
                    launch_args.push(KernelLaunchArg::F32(value_to_f64(arg) as f32));
                }
                KernelParamType::F64 => {
                    launch_args.push(KernelLaunchArg::F64(value_to_f64(arg)));
                }
                KernelParamType::I32 => {
                    launch_args.push(KernelLaunchArg::I32(value_to_i32(arg)));
                }
            }
        }
        if all_vram && backend.launch_custom_kernel(&kernel.program, grid, block, &launch_args)? {
            // Mark only output/mutated buffer tensors host-stale so subsequent GPU ops remain 0-copy!
            for &(pidx, ref t) in &buf_tensors {
                if kernel.written_buf_params().contains(&pidx) {
                    t.mark_host_stale();
                }
            }
            return Ok(());
        }
    }

    // 3. CPU / emulated execution path (bounds-checked, zero unsafe).
    let mut cpu_args: Vec<CpuParamBuf> = Vec::with_capacity(kernel.params().len());
    for (param, arg) in kernel.params().iter().zip(args.iter()) {
        match param.ty {
            KernelParamType::BufF32 => {
                let Value::Tensor(t) = arg else {
                    unreachable!();
                };
                let f64s = t.to_flat_vec();
                cpu_args.push(CpuParamBuf::F32(
                    f64s.into_iter().map(|v| v as f32).collect(),
                ));
            }
            KernelParamType::BufF64 => {
                let Value::Tensor(t) = arg else {
                    unreachable!();
                };
                cpu_args.push(CpuParamBuf::F64(t.to_flat_vec()));
            }
            KernelParamType::F32 => {
                cpu_args.push(CpuParamBuf::Scalar(
                    CpuScalar::F32(value_to_f64(arg) as f32),
                ));
            }
            KernelParamType::F64 => {
                cpu_args.push(CpuParamBuf::Scalar(CpuScalar::F64(value_to_f64(arg))));
            }
            KernelParamType::I32 => {
                cpu_args.push(CpuParamBuf::Scalar(CpuScalar::I32(value_to_i32(arg))));
            }
        }
    }

    for bid in 0..grid as i32 {
        for tid in 0..block as i32 {
            let gid = bid.wrapping_mul(block as i32).wrapping_add(tid);
            let mut locals: HashMap<String, CpuScalar> = HashMap::new();
            eval_cpu_stmts(
                &kernel.program.body,
                gid,
                bid,
                tid,
                block as i32,
                grid as i32,
                &mut cpu_args,
                &mut locals,
                kernel.name(),
            )?;
        }
    }

    // Commit mutated buffers back to host Tensor storage (and sync VRAM if host-emulated fallback ran).
    for &(pidx, ref t) in &buf_tensors {
        if !kernel.written_buf_params().contains(&pidx) {
            continue;
        }
        match &cpu_args[pidx] {
            CpuParamBuf::F32(vals) => {
                t.overwrite_contiguous_f32(vals)?;
            }
            CpuParamBuf::F64(vals) => {
                t.overwrite_contiguous_f64(vals)?;
            }
            CpuParamBuf::Scalar(_) => {}
        }
    }
    Ok(())
}

fn value_to_f64(v: &crate::value::Value) -> f64 {
    match v {
        crate::value::Value::Float(f) => *f,
        crate::value::Value::Int(n) => *n as f64,
        crate::value::Value::Scalar(s) => s.as_f64(),
        _ => 0.0,
    }
}

fn value_to_i32(v: &crate::value::Value) -> i32 {
    match v {
        crate::value::Value::Int(n) => *n as i32,
        crate::value::Value::Scalar(s) => s.as_f64() as i32,
        _ => 0,
    }
}

#[allow(clippy::too_many_arguments)]
fn eval_cpu_stmts(
    stmts: &[KernelStmt],
    gid: i32,
    bid: i32,
    tid: i32,
    bdim: i32,
    gdim: i32,
    args: &mut [CpuParamBuf],
    locals: &mut HashMap<String, CpuScalar>,
    kernel_name: &str,
) -> MoraResult<()> {
    for s in stmts {
        match s {
            KernelStmt::Let { name, expr, .. } => {
                let v = eval_cpu_expr(expr, gid, bid, tid, bdim, gdim, args, locals, kernel_name)?;
                locals.insert(name.clone(), v);
            }
            KernelStmt::Assign { name, expr } => {
                let v = eval_cpu_expr(expr, gid, bid, tid, bdim, gdim, args, locals, kernel_name)?;
                locals.insert(name.clone(), v);
            }
            KernelStmt::Store {
                buf_param,
                index,
                value,
            } => {
                let CpuScalar::I32(idx) =
                    eval_cpu_expr(index, gid, bid, tid, bdim, gdim, args, locals, kernel_name)?
                else {
                    return Err(MoraError::Runtime(format!(
                        "kernel '{kernel_name}': non-i32 store index"
                    )));
                };
                let val =
                    eval_cpu_expr(value, gid, bid, tid, bdim, gdim, args, locals, kernel_name)?;
                if idx < 0 {
                    return Err(MoraError::Runtime(format!(
                        "kernel '{kernel_name}': negative buffer store index {idx}"
                    )));
                }
                let ui = idx as usize;
                match (&mut args[*buf_param], val) {
                    (CpuParamBuf::F32(buf), CpuScalar::F32(v)) => {
                        if ui >= buf.len() {
                            return Err(MoraError::Runtime(format!(
                                "kernel '{kernel_name}': store index {ui} out of bounds (len {})",
                                buf.len()
                            )));
                        }
                        buf[ui] = v;
                    }
                    (CpuParamBuf::F64(buf), CpuScalar::F64(v)) => {
                        if ui >= buf.len() {
                            return Err(MoraError::Runtime(format!(
                                "kernel '{kernel_name}': store index {ui} out of bounds (len {})",
                                buf.len()
                            )));
                        }
                        buf[ui] = v;
                    }
                    _ => {
                        return Err(MoraError::Runtime(format!(
                            "kernel '{kernel_name}': mismatched store dtype"
                        )));
                    }
                }
            }
            KernelStmt::If {
                cond,
                then_body,
                else_body,
            } => {
                let CpuScalar::Bool(c) =
                    eval_cpu_expr(cond, gid, bid, tid, bdim, gdim, args, locals, kernel_name)?
                else {
                    return Err(MoraError::Runtime(format!(
                        "kernel '{kernel_name}': non-bool if condition"
                    )));
                };
                if c {
                    eval_cpu_stmts(
                        then_body,
                        gid,
                        bid,
                        tid,
                        bdim,
                        gdim,
                        args,
                        locals,
                        kernel_name,
                    )?;
                } else {
                    eval_cpu_stmts(
                        else_body,
                        gid,
                        bid,
                        tid,
                        bdim,
                        gdim,
                        args,
                        locals,
                        kernel_name,
                    )?;
                }
            }
            KernelStmt::While { cond, body } => {
                let mut steps = 0usize;
                loop {
                    steps += 1;
                    if steps > 10_000_000 {
                        return Err(MoraError::Runtime(format!(
                            "kernel '{kernel_name}': while loop exceeded 10M iterations on CPU"
                        )));
                    }
                    let CpuScalar::Bool(c) =
                        eval_cpu_expr(cond, gid, bid, tid, bdim, gdim, args, locals, kernel_name)?
                    else {
                        return Err(MoraError::Runtime(format!(
                            "kernel '{kernel_name}': non-bool while condition"
                        )));
                    };
                    if !c {
                        break;
                    }
                    eval_cpu_stmts(body, gid, bid, tid, bdim, gdim, args, locals, kernel_name)?;
                }
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn eval_cpu_expr(
    expr: &KernelExpr,
    gid: i32,
    bid: i32,
    tid: i32,
    bdim: i32,
    gdim: i32,
    args: &[CpuParamBuf],
    locals: &HashMap<String, CpuScalar>,
    kernel_name: &str,
) -> MoraResult<CpuScalar> {
    match expr {
        KernelExpr::ConstI32(v) => Ok(CpuScalar::I32(*v)),
        KernelExpr::ConstF32(v) => Ok(CpuScalar::F32(*v)),
        KernelExpr::ConstF64(v) => Ok(CpuScalar::F64(*v)),
        KernelExpr::ConstBool(b) => Ok(CpuScalar::Bool(*b)),
        KernelExpr::Param(idx, _) => match args[*idx] {
            CpuParamBuf::Scalar(s) => Ok(s),
            _ => Err(MoraError::Runtime(format!(
                "kernel '{kernel_name}': buffer param used as scalar"
            ))),
        },
        KernelExpr::Var(name, _) => locals.get(name).copied().ok_or_else(|| {
            MoraError::Runtime(format!("kernel '{kernel_name}': undefined local '{name}'"))
        }),
        KernelExpr::GlobalId => Ok(CpuScalar::I32(gid)),
        KernelExpr::BlockIdx => Ok(CpuScalar::I32(bid)),
        KernelExpr::ThreadIdx => Ok(CpuScalar::I32(tid)),
        KernelExpr::BlockDim => Ok(CpuScalar::I32(bdim)),
        KernelExpr::GridDim => Ok(CpuScalar::I32(gdim)),
        KernelExpr::Load {
            buf_param, index, ..
        } => {
            let CpuScalar::I32(idx) =
                eval_cpu_expr(index, gid, bid, tid, bdim, gdim, args, locals, kernel_name)?
            else {
                return Err(MoraError::Runtime(format!(
                    "kernel '{kernel_name}': non-i32 load index"
                )));
            };
            if idx < 0 {
                return Err(MoraError::Runtime(format!(
                    "kernel '{kernel_name}': negative buffer load index {idx}"
                )));
            }
            let ui = idx as usize;
            match &args[*buf_param] {
                CpuParamBuf::F32(buf) => {
                    buf.get(ui).copied().map(CpuScalar::F32).ok_or_else(|| {
                        MoraError::Runtime(format!(
                            "kernel '{kernel_name}': load index {ui} out of bounds (len {})",
                            buf.len()
                        ))
                    })
                }
                CpuParamBuf::F64(buf) => {
                    buf.get(ui).copied().map(CpuScalar::F64).ok_or_else(|| {
                        MoraError::Runtime(format!(
                            "kernel '{kernel_name}': load index {ui} out of bounds (len {})",
                            buf.len()
                        ))
                    })
                }
                CpuParamBuf::Scalar(_) => Err(MoraError::Runtime(format!(
                    "kernel '{kernel_name}': scalar param indexed as buffer"
                ))),
            }
        }
        KernelExpr::Unary { op, arg, .. } => {
            let v = eval_cpu_expr(arg, gid, bid, tid, bdim, gdim, args, locals, kernel_name)?;
            Ok(match (op, v) {
                (KernelUnaryOp::Neg, CpuScalar::I32(x)) => CpuScalar::I32(x.wrapping_neg()),
                (KernelUnaryOp::Neg, CpuScalar::F32(x)) => CpuScalar::F32(-x),
                (KernelUnaryOp::Neg, CpuScalar::F64(x)) => CpuScalar::F64(-x),
                (KernelUnaryOp::Not, CpuScalar::Bool(b)) => CpuScalar::Bool(!b),
                (KernelUnaryOp::Abs, CpuScalar::I32(x)) => CpuScalar::I32(x.wrapping_abs()),
                (KernelUnaryOp::Abs, CpuScalar::F32(x)) => CpuScalar::F32(x.abs()),
                (KernelUnaryOp::Abs, CpuScalar::F64(x)) => CpuScalar::F64(x.abs()),
                (KernelUnaryOp::Exp, CpuScalar::F32(x)) => CpuScalar::F32(x.exp()),
                (KernelUnaryOp::Exp, CpuScalar::F64(x)) => CpuScalar::F64(x.exp()),
                (KernelUnaryOp::Sqrt, CpuScalar::F32(x)) => CpuScalar::F32(x.sqrt()),
                (KernelUnaryOp::Sqrt, CpuScalar::F64(x)) => CpuScalar::F64(x.sqrt()),
                (KernelUnaryOp::Tanh, CpuScalar::F32(x)) => CpuScalar::F32(x.tanh()),
                (KernelUnaryOp::Tanh, CpuScalar::F64(x)) => CpuScalar::F64(x.tanh()),
                (KernelUnaryOp::ToF32, CpuScalar::I32(x)) => CpuScalar::F32(x as f32),
                (KernelUnaryOp::ToF32, CpuScalar::F32(x)) => CpuScalar::F32(x),
                (KernelUnaryOp::ToF32, CpuScalar::F64(x)) => CpuScalar::F32(x as f32),
                (KernelUnaryOp::ToF64, CpuScalar::I32(x)) => CpuScalar::F64(x as f64),
                (KernelUnaryOp::ToF64, CpuScalar::F32(x)) => CpuScalar::F64(x as f64),
                (KernelUnaryOp::ToF64, CpuScalar::F64(x)) => CpuScalar::F64(x),
                (KernelUnaryOp::ToI32, CpuScalar::I32(x)) => CpuScalar::I32(x),
                (KernelUnaryOp::ToI32, CpuScalar::F32(x)) => CpuScalar::I32(x as i32),
                (KernelUnaryOp::ToI32, CpuScalar::F64(x)) => CpuScalar::I32(x as i32),
                _ => v,
            })
        }
        KernelExpr::Binary { op, lhs, rhs, .. } => {
            let l = eval_cpu_expr(lhs, gid, bid, tid, bdim, gdim, args, locals, kernel_name)?;
            let r = eval_cpu_expr(rhs, gid, bid, tid, bdim, gdim, args, locals, kernel_name)?;
            Ok(match (op, l, r) {
                (KernelBinOp::Add, CpuScalar::I32(a), CpuScalar::I32(b)) => {
                    CpuScalar::I32(a.wrapping_add(b))
                }
                (KernelBinOp::Add, CpuScalar::F32(a), CpuScalar::F32(b)) => CpuScalar::F32(a + b),
                (KernelBinOp::Add, CpuScalar::F64(a), CpuScalar::F64(b)) => CpuScalar::F64(a + b),
                (KernelBinOp::Sub, CpuScalar::I32(a), CpuScalar::I32(b)) => {
                    CpuScalar::I32(a.wrapping_sub(b))
                }
                (KernelBinOp::Sub, CpuScalar::F32(a), CpuScalar::F32(b)) => CpuScalar::F32(a - b),
                (KernelBinOp::Sub, CpuScalar::F64(a), CpuScalar::F64(b)) => CpuScalar::F64(a - b),
                (KernelBinOp::Mul, CpuScalar::I32(a), CpuScalar::I32(b)) => {
                    CpuScalar::I32(a.wrapping_mul(b))
                }
                (KernelBinOp::Mul, CpuScalar::F32(a), CpuScalar::F32(b)) => CpuScalar::F32(a * b),
                (KernelBinOp::Mul, CpuScalar::F64(a), CpuScalar::F64(b)) => CpuScalar::F64(a * b),
                (KernelBinOp::Div, CpuScalar::I32(a), CpuScalar::I32(b)) => {
                    if b == 0 {
                        return Err(MoraError::Runtime(format!(
                            "kernel '{kernel_name}': integer division by zero"
                        )));
                    }
                    CpuScalar::I32(a.wrapping_div(b))
                }
                (KernelBinOp::Div, CpuScalar::F32(a), CpuScalar::F32(b)) => CpuScalar::F32(a / b),
                (KernelBinOp::Div, CpuScalar::F64(a), CpuScalar::F64(b)) => CpuScalar::F64(a / b),
                (KernelBinOp::Rem, CpuScalar::I32(a), CpuScalar::I32(b)) => {
                    if b == 0 {
                        return Err(MoraError::Runtime(format!(
                            "kernel '{kernel_name}': integer modulo by zero"
                        )));
                    }
                    CpuScalar::I32(a.wrapping_rem(b))
                }
                (KernelBinOp::Min, CpuScalar::I32(a), CpuScalar::I32(b)) => {
                    CpuScalar::I32(a.min(b))
                }
                (KernelBinOp::Min, CpuScalar::F32(a), CpuScalar::F32(b)) => {
                    CpuScalar::F32(a.min(b))
                }
                (KernelBinOp::Min, CpuScalar::F64(a), CpuScalar::F64(b)) => {
                    CpuScalar::F64(a.min(b))
                }
                (KernelBinOp::Max, CpuScalar::I32(a), CpuScalar::I32(b)) => {
                    CpuScalar::I32(a.max(b))
                }
                (KernelBinOp::Max, CpuScalar::F32(a), CpuScalar::F32(b)) => {
                    CpuScalar::F32(a.max(b))
                }
                (KernelBinOp::Max, CpuScalar::F64(a), CpuScalar::F64(b)) => {
                    CpuScalar::F64(a.max(b))
                }
                (KernelBinOp::Lt, CpuScalar::I32(a), CpuScalar::I32(b)) => CpuScalar::Bool(a < b),
                (KernelBinOp::Lt, CpuScalar::F32(a), CpuScalar::F32(b)) => CpuScalar::Bool(a < b),
                (KernelBinOp::Lt, CpuScalar::F64(a), CpuScalar::F64(b)) => CpuScalar::Bool(a < b),
                (KernelBinOp::Le, CpuScalar::I32(a), CpuScalar::I32(b)) => CpuScalar::Bool(a <= b),
                (KernelBinOp::Le, CpuScalar::F32(a), CpuScalar::F32(b)) => CpuScalar::Bool(a <= b),
                (KernelBinOp::Le, CpuScalar::F64(a), CpuScalar::F64(b)) => CpuScalar::Bool(a <= b),
                (KernelBinOp::Gt, CpuScalar::I32(a), CpuScalar::I32(b)) => CpuScalar::Bool(a > b),
                (KernelBinOp::Gt, CpuScalar::F32(a), CpuScalar::F32(b)) => CpuScalar::Bool(a > b),
                (KernelBinOp::Gt, CpuScalar::F64(a), CpuScalar::F64(b)) => CpuScalar::Bool(a > b),
                (KernelBinOp::Ge, CpuScalar::I32(a), CpuScalar::I32(b)) => CpuScalar::Bool(a >= b),
                (KernelBinOp::Ge, CpuScalar::F32(a), CpuScalar::F32(b)) => CpuScalar::Bool(a >= b),
                (KernelBinOp::Ge, CpuScalar::F64(a), CpuScalar::F64(b)) => CpuScalar::Bool(a >= b),
                (KernelBinOp::Eq, CpuScalar::I32(a), CpuScalar::I32(b)) => CpuScalar::Bool(a == b),
                (KernelBinOp::Eq, CpuScalar::F32(a), CpuScalar::F32(b)) => CpuScalar::Bool(a == b),
                (KernelBinOp::Eq, CpuScalar::F64(a), CpuScalar::F64(b)) => CpuScalar::Bool(a == b),
                (KernelBinOp::Eq, CpuScalar::Bool(a), CpuScalar::Bool(b)) => {
                    CpuScalar::Bool(a == b)
                }
                (KernelBinOp::Ne, CpuScalar::I32(a), CpuScalar::I32(b)) => CpuScalar::Bool(a != b),
                (KernelBinOp::Ne, CpuScalar::F32(a), CpuScalar::F32(b)) => CpuScalar::Bool(a != b),
                (KernelBinOp::Ne, CpuScalar::F64(a), CpuScalar::F64(b)) => CpuScalar::Bool(a != b),
                (KernelBinOp::Ne, CpuScalar::Bool(a), CpuScalar::Bool(b)) => {
                    CpuScalar::Bool(a != b)
                }
                (KernelBinOp::And, CpuScalar::Bool(a), CpuScalar::Bool(b)) => {
                    CpuScalar::Bool(a && b)
                }
                (KernelBinOp::Or, CpuScalar::Bool(a), CpuScalar::Bool(b)) => {
                    CpuScalar::Bool(a || b)
                }
                _ => {
                    return Err(MoraError::Runtime(format!(
                        "kernel '{kernel_name}': mismatched CPU binary operands"
                    )));
                }
            })
        }
    }
}
