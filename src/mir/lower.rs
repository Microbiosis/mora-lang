//! MirExpr → MIR lowering (v0.55: V3 pipeline only)
//!
//! 全部 lowering 从 `Vec<MirExpr>` 直接构造 `MirFunction`。
//! 老的 `lower_program(&[NodeId], &AstArena)` (AST v2 路径) 在 v0.55 删除。
//! 老的 `Lowerer` struct (882 行遗留实现) 在 v0.55 删除。
//!
//! 入口:
//! - `lower_mir_exprs(exprs: &[MirExpr]) -> Result<MirFunction, String>`
//! - `typecheck_mir_exprs(exprs: &mut [MirExpr]) -> Vec<TypeError>`

// ── MirExpr-based lowering (v0.55: V3 pipeline) ──

use super::{Label, MirFunction, MirInst, Reg};
use crate::mir::expr::MirExpr;

/// 对 MirExpr 列表做类型检查（委托 check_program_mir HM 推断引擎）
pub fn typecheck_mir_exprs(_exprs: &mut [MirExpr]) -> Vec<crate::typeck::TypeError> {
    crate::typeck::check_program_mir(_exprs)
}

/// v0.75.30: 显式编译选项变体 — 调用方（CLI 编译入口）显式指定优化等级，
/// 不读环境变量。语义与 `lower_mir_exprs` 完全一致，仅优化等级来源不同。
/// REPL/import/pregel 等动态路径继续走 `lower_mir_exprs`（env 兜底）。
pub fn lower_mir_exprs_with_opt(
    exprs: &[MirExpr],
    opt_level: crate::mir::ssa::OptLevel,
) -> Result<MirFunction, String> {
    let mut l = MirExprLowerer::new();
    for expr in exprs {
        let _dst = l.lower_expr(expr)?;
    }
    let mut func = l.finish();
    // v0.58: Cascades 优化 pass
    crate::mir::optimize::apply_rules(&mut func);
    // v0.75.7: SSA 优化管线（显式等级 or MORA_OPT=1/2 启用，默认关闭 —
    // 热路径零开销）。rename 根因修复后（Define/Assign src 参与 rename），
    // 等价性测试全绿。
    if opt_level.enabled() {
        crate::mir::opt::optimize(&mut func, opt_level);
    }
    Ok(func)
}

/// 将 MirExpr 列表 lowering 为 MirFunction（env 兜底：CLI 未显式 `--opt`
/// 时沿用 MORA_OPT — REPL/import/pregel 等无编译命令的动态路径）。
pub fn lower_mir_exprs(exprs: &[MirExpr]) -> Result<MirFunction, String> {
    lower_mir_exprs_with_opt(exprs, crate::mir::ssa::OptLevel::from_env())
}

/// v0.75.39: 共享 emit 机制 — MirExprLowerer 与 ParserV3 单遍编译共用。
///
/// alloc_reg（bump 分配）/ emit（Vec push）/ patch_label_at（label 回填）
/// 是三个自包含原语，不依赖 MirExpr 任何执行语义。阶段 3 parser 直接
/// emit 时复用同一套机制（label 即 insts 索引，patch 即覆盖）。
#[derive(Default)]
pub struct EmitContext {
    pub next_reg: Reg,
    pub insts: Vec<MirInst>,
    /// 循环上下文栈: (continue_label, break_label)
    pub loop_stack: Vec<(Label, Label)>,
}

impl EmitContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn alloc_reg(&mut self) -> Reg {
        let r = self.next_reg;
        self.next_reg += 1;
        r
    }

    pub fn emit(&mut self, inst: MirInst) {
        self.insts.push(inst);
    }

    pub fn patch_label_at(&mut self, idx: usize, label: Label) {
        match &mut self.insts[idx] {
            MirInst::JumpIfNot(_, lbl) | MirInst::JumpIf(_, lbl) | MirInst::Jump(lbl) => {
                *lbl = label;
            }
            _ => {}
        }
    }

    pub fn finish(self) -> MirFunction {
        MirFunction {
            params: vec![],
            body: self.insts,
            n_regs: self.next_reg,
        }
    }
}

/// MirExpr → MIR 指令 lowering（v0.55 完整版）
struct MirExprLowerer {
    emit: EmitContext,
}

impl MirExprLowerer {
    fn new() -> Self {
        Self {
            emit: EmitContext::new(),
        }
    }

    fn alloc_reg(&mut self) -> Reg {
        self.emit.alloc_reg()
    }

    fn emit(&mut self, inst: MirInst) {
        self.emit.emit(inst);
    }

    fn patch_label_at(&mut self, idx: usize, label: Label) {
        self.emit.patch_label_at(idx, label);
    }

    fn finish(self) -> MirFunction {
        self.emit.finish()
    }

    /// Lower expression → returns result register
    fn lower_expr(&mut self, expr: &MirExpr) -> Result<Reg, String> {
        use crate::common::Literal;
        use crate::mir::expr::{MirCallee, MirExprKind};

        match &expr.kind {
            // ── Literals ──
            MirExprKind::Literal(Literal::Int(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Int(*v)));
                Ok(dst)
            }
            MirExprKind::Literal(Literal::Float(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Float(*v)));
                Ok(dst)
            }
            MirExprKind::Literal(Literal::String(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::String(v.clone())));
                Ok(dst)
            }
            MirExprKind::Literal(Literal::Bool(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Bool(*v)));
                Ok(dst)
            }
            MirExprKind::Literal(Literal::Char(c, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Char(*c)));
                Ok(dst)
            }
            MirExprKind::Literal(Literal::Nil(_)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Variables ──
            MirExprKind::Variable(name) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Var(dst, name.clone()));
                Ok(dst)
            }

            // ── Binary operations ──
            MirExprKind::Binary { left, op, right } => {
                let l = self.lower_expr(left)?;
                let r = self.lower_expr(right)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::BinaryOp(dst, l, op.clone(), r));
                Ok(dst)
            }

            // ── Logical And/Or (short-circuit) ──
            MirExprKind::And { left, right } => {
                let l = self.lower_expr(left)?;
                let dst = self.alloc_reg();
                // If left is false, skip right
                self.emit(MirInst::JumpIfNot(l, 0)); // placeholder
                let jump_idx = self.emit.insts.len() - 1;
                let r = self.lower_expr(right)?;
                self.emit(MirInst::BinaryOp(dst, l, crate::common::BinaryOp::Equal, r));
                let end = self.emit.insts.len();
                self.patch_label_at(jump_idx, end);
                // If short-circuited, dst is false (copy l)
                Ok(dst)
            }
            MirExprKind::Or { left, right } => {
                let l = self.lower_expr(left)?;
                let dst = self.alloc_reg();
                // If left is true, skip right
                self.emit(MirInst::JumpIf(l, 0)); // placeholder
                let jump_idx = self.emit.insts.len() - 1;
                let r = self.lower_expr(right)?;
                self.emit(MirInst::BinaryOp(
                    dst,
                    l,
                    crate::common::BinaryOp::NotEqual,
                    r,
                ));
                let end = self.emit.insts.len();
                self.patch_label_at(jump_idx, end);
                Ok(dst)
            }

            // ── Function calls ──
            MirExprKind::Call { callee, args } => {
                // v0.75.33: MirCallee::Method（`obj.method(args)`）走
                // MirInst::MethodCall — ParserV3 把 receiver 作为第一个参数
                // 传入，此处弹出作为 receiver 寄存器。此前拼 "obj_method"
                // mangled 字符串 → interpreter 查不到该名字 →
                // "Undefined function or task"（循环体真正执行后暴露）。
                if let MirCallee::Method(_obj, method) = callee {
                    let mut arg_regs: Vec<Reg> = Vec::new();
                    for arg in args {
                        let r = self.lower_expr(arg)?;
                        arg_regs.push(r);
                    }
                    if let Some(recv_reg) = arg_regs.first().copied() {
                        let dst = self.alloc_reg();
                        self.emit(MirInst::MethodCall(
                            dst,
                            recv_reg,
                            method.clone(),
                            arg_regs[1..].to_vec(),
                        ));
                        return Ok(dst);
                    }
                }
                let callee_name = match callee {
                    MirCallee::Name(n) => n.clone(),
                    MirCallee::Var(n) => n.clone(),
                    _ => "unknown".to_string(),
                };
                let mut arg_regs: Vec<Reg> = Vec::new();
                for arg in args {
                    let r = self.lower_expr(arg)?;
                    arg_regs.push(r);
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::Call(dst, callee_name, arg_regs));
                Ok(dst)
            }

            // ── Method calls ──
            MirExprKind::MethodCall {
                receiver,
                method,
                args,
            } => {
                let recv_reg = self.lower_expr(receiver)?;
                let mut arg_regs: Vec<Reg> = Vec::new();
                for arg in args {
                    let r = self.lower_expr(arg)?;
                    arg_regs.push(r);
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::MethodCall(dst, recv_reg, method.clone(), arg_regs));
                Ok(dst)
            }

            // ── Collections ──
            MirExprKind::List(items) => {
                let mut item_regs: Vec<Reg> = Vec::new();
                for item in items {
                    let r = self.lower_expr(item)?;
                    item_regs.push(r);
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::ListLit(dst, item_regs));
                Ok(dst)
            }
            MirExprKind::Dict(entries) => {
                let mut pair_regs: Vec<(String, Reg)> = Vec::new();
                for (key, val) in entries {
                    let r = self.lower_expr(val)?;
                    pair_regs.push((key.clone(), r));
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::DictLit(dst, pair_regs));
                Ok(dst)
            }

            // ── If/Else ──
            MirExprKind::If { cond, then, r#else } => {
                let c = self.lower_expr(cond)?;
                // v0.75.79: 与 compile 主路径对称 — if 结果经寄存器传递
                // （Copy dst=src），不再经 env 临时名 `__if_result`（Assign
                // 写未定义变量静默失败，分支值丢失）。
                self.emit(MirInst::JumpIfNot(c, 0)); // placeholder
                let jumpifnot_idx = self.emit.insts.len() - 1;

                // Then branch
                let then_dst = self.lower_expr(then)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Copy(dst, then_dst));
                // Jump to end
                self.emit(MirInst::Jump(0)); // placeholder
                let jump_end_idx = self.emit.insts.len() - 1;

                // Else branch
                let else_start = self.emit.insts.len();
                self.patch_label_at(jumpifnot_idx, else_start);
                if let Some(else_expr) = r#else {
                    let else_dst = self.lower_expr(else_expr)?;
                    self.emit(MirInst::Copy(dst, else_dst));
                } else {
                    let nil_reg = self.alloc_reg();
                    self.emit(MirInst::Const(nil_reg, crate::value::Value::Nil));
                    self.emit(MirInst::Copy(dst, nil_reg));
                }
                // End
                let end = self.emit.insts.len();
                self.patch_label_at(jump_end_idx, end);
                Ok(dst)
            }

            // ── Match ──
            MirExprKind::Match { scrutinee, arms } => {
                let val_reg = self.lower_expr(scrutinee)?;
                let match_arms: Vec<(String, Option<Reg>, Box<MirFunction>, Reg)> = arms
                    .iter()
                    .map(|arm| {
                        let pat_str = pattern_to_string(&arm.pattern);
                        let mut body_lowerer = MirExprLowerer::new();
                        let arm_val_reg = body_lowerer.lower_expr(&arm.body)?;
                        body_lowerer.emit(MirInst::Return(Some(arm_val_reg)));
                        Ok((pat_str, None, Box::new(body_lowerer.finish()), arm_val_reg))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let dst = self.alloc_reg();
                self.emit(MirInst::MatchExpr {
                    val: val_reg,
                    arms: match_arms,
                });
                Ok(dst)
            }

            // ── For loop ──
            MirExprKind::Loop {
                var,
                iterable,
                body,
            } => {
                use crate::value::Value;
                let iter_reg = self.lower_expr(iterable)?;
                // i = 0
                let i_reg = self.alloc_reg();
                self.emit(MirInst::Const(i_reg, Value::Int(0)));
                // len = len(iter)
                let len_reg = self.alloc_reg();
                self.emit(MirInst::Call(len_reg, "len".to_string(), vec![iter_reg]));
                // one = 1
                let one_reg = self.alloc_reg();
                self.emit(MirInst::Const(one_reg, Value::Int(1)));

                // loop_label: continue target
                let loop_label = self.emit.insts.len();
                // cond = i >= len
                let cond_reg = self.alloc_reg();
                self.emit(MirInst::BinaryOp(
                    cond_reg,
                    i_reg,
                    crate::common::BinaryOp::GreaterEqual,
                    len_reg,
                ));
                // if cond: goto end
                self.emit(MirInst::JumpIf(cond_reg, 0));
                let exit_jump_idx = self.emit.insts.len() - 1;

                // x = iter[i]
                let x_reg = self.alloc_reg();
                self.emit(MirInst::Index(x_reg, iter_reg, i_reg));
                self.emit(MirInst::Define(var.clone(), x_reg));

                // body
                let body_start = self.emit.insts.len();
                self.emit.loop_stack.push((loop_label, 0));
                let _ = self.lower_expr(body)?;
                self.emit.loop_stack.pop();
                let body_end = self.emit.insts.len();

                // incr: i = i + 1; goto loop
                self.emit(MirInst::BinaryOp(
                    i_reg,
                    i_reg,
                    crate::common::BinaryOp::Add,
                    one_reg,
                ));
                self.emit(MirInst::Jump(loop_label));

                // end_label: break target
                let end_label = self.emit.insts.len();
                self.patch_label_at(exit_jump_idx, end_label);
                // Patch break labels in body
                for i in body_start..body_end {
                    match &mut self.emit.insts[i] {
                        MirInst::Break(lbl) => *lbl = end_label,
                        MirInst::Continue(lbl) => *lbl = loop_label,
                        _ => {}
                    }
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, Value::Nil));
                Ok(dst)
            }

            // ── While loop ──
            MirExprKind::While { cond, body } => {
                let loop_label = self.emit.insts.len();
                let c = self.lower_expr(cond)?;
                self.emit(MirInst::JumpIfNot(c, 0)); // placeholder
                let exit_jump_idx = self.emit.insts.len() - 1;

                let body_start = self.emit.insts.len();
                self.emit.loop_stack.push((loop_label, 0));
                let _ = self.lower_expr(body)?;
                self.emit.loop_stack.pop();
                let body_end = self.emit.insts.len();

                self.emit(MirInst::Jump(loop_label));
                let end_label = self.emit.insts.len();
                self.patch_label_at(exit_jump_idx, end_label);
                // Patch break/continue
                for i in body_start..body_end {
                    match &mut self.emit.insts[i] {
                        MirInst::Break(lbl) => *lbl = end_label,
                        MirInst::Continue(lbl) => *lbl = loop_label,
                        _ => {}
                    }
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Closure ──
            MirExprKind::Closure { params, body, .. } => {
                let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                let mut body_lowerer = MirExprLowerer::new();
                let body_dst = body_lowerer.lower_expr(body)?;
                body_lowerer.emit(MirInst::Return(Some(body_dst)));
                let body_mir = body_lowerer.finish();
                let dst = self.alloc_reg();
                self.emit(MirInst::Closure {
                    dst,
                    params: param_names,
                    body: Box::new(body_mir),
                });
                Ok(dst)
            }

            // ── FnDef ──
            MirExprKind::FnDef {
                name, params, body, ..
            } => {
                let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                let mut body_lowerer = MirExprLowerer::new();
                let body_dst = body_lowerer.lower_expr(body)?;
                body_lowerer.emit(MirInst::Return(Some(body_dst)));
                let body_mir = body_lowerer.finish();
                // v0.75.79: TaskDef 无 dst 字段 — 不分配死寄存器（顶层结果被
                // lower_mir_exprs_with_opt 的 `_dst` 丢弃）。修复前 alloc_reg
                // 使 n_regs 比 compile 主路径多 1（差分等价断言暴露）。
                self.emit(MirInst::TaskDef {
                    name: name.clone(),
                    params: param_names,
                    body: Box::new(body_mir),
                });
                Ok(0)
            }

            // ── DynTrait ──
            MirExprKind::DynTrait {
                expr,
                trait_name,
                generics,
            } => {
                let src = self.lower_expr(expr)?;
                let dst = self.alloc_reg();
                let generic_strs: Vec<String> = generics.iter().map(|t| t.name()).collect();
                self.emit(MirInst::DynTrait {
                    dst,
                    src,
                    trait_generics: generic_strs,
                    trait_name: trait_name.clone(),
                });
                Ok(dst)
            }

            // ── Prompt ──
            MirExprKind::Prompt { parts } => {
                let mut part_regs: Vec<Reg> = Vec::new();
                for part in parts {
                    let r = self.lower_expr(part)?;
                    part_regs.push(r);
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::Prompt(dst, part_regs));
                Ok(dst)
            }

            // ── Let binding ──
            MirExprKind::LetBinding {
                name,
                value,
                init_body,
                ..
            } => {
                let v_dst = self.lower_expr(value)?;
                self.emit(MirInst::Define(name.clone(), v_dst));
                let b_dst = self.lower_expr(init_body)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Assign("__let_result".to_string(), b_dst));
                self.emit(MirInst::Var(dst, "__let_result".to_string()));
                Ok(dst)
            }

            // ── Assignment ──
            MirExprKind::Assign { target, value } => {
                let v = self.lower_expr(value)?;
                self.emit(MirInst::Assign(target.clone(), v));
                Ok(v)
            }

            // ── IndexAssign ──
            MirExprKind::IndexAssign {
                object,
                index,
                value,
            } => {
                let obj = self.lower_expr(object)?;
                let idx = self.lower_expr(index)?;
                let val = self.lower_expr(value)?;
                self.emit(MirInst::IndexAssign(obj, idx, val));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Expr (discard result) ──
            // v0.75.20: MirExprKind::Expr 已删（死变体，parser 零构造）；
            // MirInst::Expr 作为运算原语保留（手工构造可达，运行时语义不变）。
            // ── Sequence ──
            MirExprKind::Sequence(exprs) => {
                let mut last_dst = 0;
                for e in exprs {
                    last_dst = self.lower_expr(e)?;
                }
                Ok(last_dst)
            }

            // ── Return / Break / Continue ──
            MirExprKind::Return(val) => {
                match val {
                    Some(v) => {
                        let r = self.lower_expr(v)?;
                        self.emit(MirInst::Return(Some(r)));
                    }
                    None => {
                        self.emit(MirInst::Return(None));
                    }
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            MirExprKind::Break(_label) => {
                let (_, brk) = self
                    .emit
                    .loop_stack
                    .last()
                    .copied()
                    .ok_or("Break outside loop")?;
                self.emit(MirInst::Break(brk));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            MirExprKind::Continue(_label) => {
                let (cont, _) = self
                    .emit
                    .loop_stack
                    .last()
                    .copied()
                    .ok_or("Continue outside loop")?;
                self.emit(MirInst::Continue(cont));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Orchestrate ──
            MirExprKind::Orchestrate {
                input_var,
                result_var,
                kind,
            } => {
                self.emit(MirInst::Orchestrate {
                    input_var: input_var.clone(),
                    result_var: result_var.clone(),
                    kind: kind.clone(),
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Type definitions ──
            MirExprKind::TypeAlias { name, target } => {
                self.emit(MirInst::TypeAlias {
                    name: name.clone(),
                    target: target.name(),
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            MirExprKind::EnumDef { name, variants } => {
                let evs: Vec<crate::common::EnumVariant> = variants
                    .iter()
                    .map(|v| crate::common::EnumVariant {
                        name: v.clone(),
                        data: None,
                    })
                    .collect();
                self.emit(MirInst::EnumDef {
                    name: name.clone(),
                    variants: evs,
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            MirExprKind::StructDef { name, fields } => {
                let sfs: Vec<crate::common::StructField> = fields
                    .iter()
                    .map(|(fname, ftype)| crate::common::StructField {
                        name: fname.clone(),
                        type_hint: ftype.name(),
                    })
                    .collect();
                self.emit(MirInst::StructDef {
                    name: name.clone(),
                    fields: sfs,
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Import / Macro ──
            MirExprKind::Import(path) => {
                self.emit(MirInst::Import(path.clone()));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            MirExprKind::MacroDef { name, params } => {
                self.emit(MirInst::MacroDef {
                    name: name.clone(),
                    params: params.clone(),
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            } // ── Grouping (transparent) ──
              // v0.75.20: MirExprKind::Grouping 已删（mir_group 恒等函数，
              // 从未产出包裹节点；括号仅作优先级，parse 时不建节点）。
        }
    }
}

/// Convert MirExpr Pattern to string representation for MatchExpr.
/// v0.75.40: pub — ParserV3 单遍编译（emit_match_arm）复用同一序列化。
pub fn pattern_to_string(pattern: &crate::mir::expr::Pattern) -> String {
    use crate::mir::expr::Pattern;
    match pattern {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Variable(name) => name.clone(),
        Pattern::Literal(lit) => match lit {
            crate::common::Literal::String(s, _) => format!("str:{}", s),
            crate::common::Literal::Char(c, _) => format!("char:{}", c),
            crate::common::Literal::Int(i, _) => format!("int:{}", i),
            crate::common::Literal::Float(f, _) => format!("float:{}", f),
            crate::common::Literal::Bool(b, _) => format!("bool:{}", b),
            crate::common::Literal::Nil(_) => "nil".to_string(),
        },
        Pattern::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(pattern_to_string).collect();
            format!("tuple:({})", parts.join(","))
        }
        Pattern::List { head, tail } => {
            format!(
                "list:[{}|{}]",
                pattern_to_string(head),
                pattern_to_string(tail)
            )
        }
        Pattern::Dict { required, rest } => {
            let fields: Vec<String> = required
                .iter()
                .map(|(k, v)| format!("{}:{}", k, pattern_to_string(v)))
                .collect();
            let rest_str = if *rest { ",.." } else { "" };
            format!("dict:{{{}}}", fields.join(",") + rest_str)
        }
        Pattern::TypeAscription { name, pattern } => {
            format!("{}:{}", name, pattern_to_string(pattern))
        }
    }
}
