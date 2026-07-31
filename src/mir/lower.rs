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

/// 将 MirExpr 列表 lowering 为 MirFunction
pub fn lower_mir_exprs(exprs: &[MirExpr]) -> Result<MirFunction, String> {
    let mut l = MirExprLowerer::new();
    for expr in exprs {
        let _dst = l.lower_expr(expr)?;
    }
    let mut func = l.finish();
    // v0.58: Cascades 优化 pass
    crate::mir::optimize::apply_rules(&mut func);
    // v0.75.7: SSA 优化管线（MORA_OPT=1/2 启用，默认关闭 — 热路径零开销）。
    // rename 根因修复后（Define/Assign src 参与 rename），等价性测试全绿，
    // 环境变量从此真正生效。
    let opt_level = crate::mir::ssa::OptLevel::from_env();
    if opt_level.enabled() {
        crate::mir::opt::optimize(&mut func, opt_level);
    }
    Ok(func)
}

/// MirExpr → MIR 指令 lowering（v0.55 完整版）
struct MirExprLowerer {
    next_reg: Reg,
    insts: Vec<MirInst>,
    /// 循环上下文栈: (continue_label, break_label)
    loop_stack: Vec<(Label, Label)>,
}

impl MirExprLowerer {
    fn new() -> Self {
        Self {
            next_reg: 0,
            insts: Vec::new(),
            loop_stack: Vec::new(),
        }
    }

    fn finish(self) -> MirFunction {
        MirFunction {
            params: vec![],
            body: self.insts,
            n_regs: self.next_reg,
        }
    }

    fn alloc_reg(&mut self) -> Reg {
        let r = self.next_reg;
        self.next_reg += 1;
        r
    }

    fn emit(&mut self, inst: MirInst) {
        self.insts.push(inst);
    }

    fn patch_label_at(&mut self, idx: usize, label: Label) {
        match &mut self.insts[idx] {
            MirInst::JumpIfNot(_, lbl) | MirInst::JumpIf(_, lbl) | MirInst::Jump(lbl) => {
                *lbl = label;
            }
            _ => {}
        }
    }

    /// Lower expression → returns result register
    fn lower_expr(&mut self, expr: &MirExpr) -> Result<Reg, String> {
        use crate::mir::expr::{MirCallee, MirExprKind};
        use crate::common::Literal;

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
                let jump_idx = self.insts.len() - 1;
                let r = self.lower_expr(right)?;
                self.emit(MirInst::BinaryOp(dst, l, crate::common::BinaryOp::Equal, r));
                let end = self.insts.len();
                self.patch_label_at(jump_idx, end);
                // If short-circuited, dst is false (copy l)
                Ok(dst)
            }
            MirExprKind::Or { left, right } => {
                let l = self.lower_expr(left)?;
                let dst = self.alloc_reg();
                // If left is true, skip right
                self.emit(MirInst::JumpIf(l, 0)); // placeholder
                let jump_idx = self.insts.len() - 1;
                let r = self.lower_expr(right)?;
                self.emit(MirInst::BinaryOp(dst, l, crate::common::BinaryOp::NotEqual, r));
                let end = self.insts.len();
                self.patch_label_at(jump_idx, end);
                Ok(dst)
            }

            // ── Function calls ──
            MirExprKind::Call { callee, args } => {
                let callee_name = match callee {
                    MirCallee::Name(n) => n.clone(),
                    MirCallee::Var(n) => n.clone(),
                    MirCallee::Method(obj, method) => format!("{}_{}", obj, method),
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
            MirExprKind::MethodCall { receiver, method, args } => {
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

            // ── Pipe ──
            MirExprKind::Pipe { lhs, rhs } => {
                let lhs_reg = self.lower_expr(lhs)?;
                let rhs_reg = self.lower_expr(rhs)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Pipe(dst, lhs_reg, rhs_reg));
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
                // JumpIfNot to else branch
                self.emit(MirInst::JumpIfNot(c, 0)); // placeholder
                let jumpifnot_idx = self.insts.len() - 1;

                // Then branch
                let then_dst = self.lower_expr(then)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Assign("__if_result".to_string(), then_dst));
                // Jump to end
                self.emit(MirInst::Jump(0)); // placeholder
                let jump_end_idx = self.insts.len() - 1;

                // Else branch
                let else_start = self.insts.len();
                self.patch_label_at(jumpifnot_idx, else_start);
                if let Some(else_expr) = r#else {
                    let else_dst = self.lower_expr(else_expr)?;
                    self.emit(MirInst::Assign("__if_result".to_string(), else_dst));
                } else {
                    self.emit(MirInst::Assign("__if_result".to_string(), 0));
                }
                // End
                let end = self.insts.len();
                self.patch_label_at(jump_end_idx, end);
                self.emit(MirInst::Var(dst, "__if_result".to_string()));
                Ok(dst)
            }

            // ── Match ──
            MirExprKind::Match { scrutinee, arms } => {
                let val_reg = self.lower_expr(scrutinee)?;
                let match_arms: Vec<(String, Option<Reg>, Box<MirFunction>, Reg)> = arms
                    .iter()
                    .map(|arm| {
                        let pat_str = mir_pattern_to_string(&arm.pattern);
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
            MirExprKind::Loop { var, iterable, body } => {
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
                let loop_label = self.insts.len();
                // cond = i >= len
                let cond_reg = self.alloc_reg();
                self.emit(MirInst::BinaryOp(cond_reg, i_reg, crate::common::BinaryOp::GreaterEqual, len_reg));
                // if cond: goto end
                self.emit(MirInst::JumpIf(cond_reg, 0));
                let exit_jump_idx = self.insts.len() - 1;

                // x = iter[i]
                let x_reg = self.alloc_reg();
                self.emit(MirInst::Index(x_reg, iter_reg, i_reg));
                self.emit(MirInst::Define(var.clone(), x_reg));

                // body
                let body_start = self.insts.len();
                self.loop_stack.push((loop_label, 0));
                let _ = self.lower_expr(body)?;
                self.loop_stack.pop();
                let body_end = self.insts.len();

                // incr: i = i + 1; goto loop
                self.emit(MirInst::BinaryOp(i_reg, i_reg, crate::common::BinaryOp::Add, one_reg));
                self.emit(MirInst::Jump(loop_label));

                // end_label: break target
                let end_label = self.insts.len();
                self.patch_label_at(exit_jump_idx, end_label);
                // Patch break labels in body
                for i in body_start..body_end {
                    match &mut self.insts[i] {
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
                let loop_label = self.insts.len();
                let c = self.lower_expr(cond)?;
                self.emit(MirInst::JumpIfNot(c, 0)); // placeholder
                let exit_jump_idx = self.insts.len() - 1;

                let body_start = self.insts.len();
                self.loop_stack.push((loop_label, 0));
                let _ = self.lower_expr(body)?;
                self.loop_stack.pop();
                let body_end = self.insts.len();

                self.emit(MirInst::Jump(loop_label));
                let end_label = self.insts.len();
                self.patch_label_at(exit_jump_idx, end_label);
                // Patch break/continue
                for i in body_start..body_end {
                    match &mut self.insts[i] {
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
            MirExprKind::FnDef { name, params, body, .. } => {
                let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                let mut body_lowerer = MirExprLowerer::new();
                let body_dst = body_lowerer.lower_expr(body)?;
                body_lowerer.emit(MirInst::Return(Some(body_dst)));
                let body_mir = body_lowerer.finish();
                let dst = self.alloc_reg();
                self.emit(MirInst::TaskDef {
                    name: name.clone(),
                    params: param_names,
                    body: Box::new(body_mir),
                });
                Ok(dst)
            }

            // ── DynTrait ──
            MirExprKind::DynTrait { expr, trait_name, generics } => {
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
            MirExprKind::LetBinding { name, value, init_body, .. } => {
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
            MirExprKind::IndexAssign { object, index, value } => {
                let obj = self.lower_expr(object)?;
                let idx = self.lower_expr(index)?;
                let val = self.lower_expr(value)?;
                self.emit(MirInst::IndexAssign(obj, idx, val));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Expr (discard result) ──
            MirExprKind::Expr(inner) => {
                let r = self.lower_expr(inner)?;
                self.emit(MirInst::Expr(r));
                Ok(r)
            }

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
                let (_, brk) = self.loop_stack.last().copied().ok_or("Break outside loop")?;
                self.emit(MirInst::Break(brk));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            MirExprKind::Continue(_label) => {
                let (cont, _) = self.loop_stack.last().copied().ok_or("Continue outside loop")?;
                self.emit(MirInst::Continue(cont));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Orchestrate ──
            MirExprKind::Orchestrate { input_var, result_var, kind } => {
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
                    .map(|v| crate::common::EnumVariant { name: v.clone(), data: None })
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
            }

            // ── Grouping (transparent) ──
            MirExprKind::Grouping(inner) => {
                self.lower_expr(inner)
            }
        }
    }
}

/// Convert MirExpr Pattern to string representation for MatchExpr
fn mir_pattern_to_string(pattern: &crate::mir::expr::Pattern) -> String {
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
            let parts: Vec<String> = items.iter().map(mir_pattern_to_string).collect();
            format!("tuple:({})", parts.join(","))
        }
        Pattern::List { head, tail } => {
            format!("list:[{}|{}]", mir_pattern_to_string(head), mir_pattern_to_string(tail))
        }
        Pattern::Dict { required, rest } => {
            let fields: Vec<String> = required
                .iter()
                .map(|(k, v)| format!("{}:{}", k, mir_pattern_to_string(v)))
                .collect();
            let rest_str = if *rest { ",.." } else { "" };
            format!("dict:{{{}}}", fields.join(",") + rest_str)
        }
        Pattern::TypeAscription { name, pattern } => {
            format!("{}:{}", name, mir_pattern_to_string(pattern))
        }
    }
}
