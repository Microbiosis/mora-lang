//! AST → MIR lowering pass（α.0）
//!
//! 遍历 ASTv2，为每个表达式分配虚拟寄存器，生成线性 MIR 指令序列。
//! 控制流（If/For）展平为 Label + Jump。FlowSignal 枚举传返 → Jump/Return/Break/Continue 指令。

use crate::ast_v2::{AstArena, ExprKind, NodeId, StmtKind};
use crate::common::BinaryOp;
use crate::flow::literal_to_value_static;

use super::{Label, MirFunction, MirInst, Reg};

/// 把一段顶层程序（stmt_ids）lowering 成 MirFunction
pub fn lower_program(stmt_ids: &[NodeId], arena: &AstArena) -> Result<MirFunction, String> {
    let mut l = Lowerer::new();
    for sid in stmt_ids {
        l.lower_stmt(*sid, arena)?;
    }
    Ok(l.finish())
}

struct Lowerer {
    next_reg: Reg,
    insts: Vec<MirInst>,
    // α.1: 循环上下文栈——Break/Continue 跳转目标
    loop_stack: Vec<(Label, Label)>, // (continue_label, break_label)
                                     // α.0 简化：label 直接是分配时的 insts.len()，Jump 引用它
}

impl Lowerer {
    fn new() -> Self {
        Self {
            next_reg: 0,
            insts: Vec::new(),
            loop_stack: Vec::new(),
        }
    }

    fn finish(self) -> MirFunction {
        MirFunction {
            params: Vec::new(),
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

    // ── 表达式 lowering：返回结果所在的寄存器 ──

    fn lower_expr(&mut self, eid: NodeId, arena: &AstArena) -> Result<Reg, String> {
        let expr = arena
            .get_expr(eid)
            .ok_or_else(|| format!("lower_expr: NodeId {} not in arena", eid.0))?;
        match &expr.kind {
            ExprKind::Literal(lit) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, literal_to_value_static(lit)));
                Ok(dst)
            }
            ExprKind::Variable(name) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Var(dst, name.clone()));
                Ok(dst)
            }
            ExprKind::Binary { left, op, right } => {
                let l = self.lower_expr(*left, arena)?;
                let r = self.lower_expr(*right, arena)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::BinaryOp(dst, l, op.clone(), r));
                Ok(dst)
            }
            ExprKind::Call { callee, args } => {
                let arg_regs: Vec<Reg> = args
                    .iter()
                    .map(|a| self.lower_expr(*a, arena))
                    .collect::<Result<_, _>>()?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Call(dst, callee.clone(), arg_regs));
                Ok(dst)
            }
            ExprKind::Grouping(inner) => {
                // 分组表达式：透传内部表达式
                self.lower_expr(*inner, arena)
            }
            // α.1: 列表字面量
            ExprKind::List(items) => {
                let item_regs: Vec<Reg> = items
                    .iter()
                    .map(|i| self.lower_expr(*i, arena))
                    .collect::<Result<_, _>>()?;
                let dst = self.alloc_reg();
                self.emit(MirInst::ListLit(dst, item_regs));
                Ok(dst)
            }
            // α.1: 字典字面量（key 是 String，val 是 NodeId）
            ExprKind::Dict(pairs) => {
                let pair_regs: Vec<(String, Reg)> = pairs
                    .iter()
                    .map(|(k, v)| self.lower_expr(*v, arena).map(|r| (k.clone(), r)))
                    .collect::<Result<_, _>>()?;
                let dst = self.alloc_reg();
                self.emit(MirInst::DictLit(dst, pair_regs));
                Ok(dst)
            }
            // α.1: 索引 obj[idx]
            ExprKind::Index { object, index } => {
                let obj_reg = self.lower_expr(*object, arena)?;
                let idx_reg = self.lower_expr(*index, arena)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Index(dst, obj_reg, idx_reg));
                Ok(dst)
            }
            // α.1: 方法调用 recv.method(args)
            ExprKind::MethodCall {
                object,
                method,
                args,
            } => {
                let recv_reg = self.lower_expr(*object, arena)?;
                let arg_regs: Vec<Reg> = args
                    .iter()
                    .map(|a| self.lower_expr(*a, arena))
                    .collect::<Result<_, _>>()?;
                let dst = self.alloc_reg();
                self.emit(MirInst::MethodCall(dst, recv_reg, method.clone(), arg_regs));
                Ok(dst)
            }
            // α.1: 管道 lhs |> rhs（rhs 是可调用表达式，通常是 Variable 或 Call）
            ExprKind::Pipe { left, right } => {
                let lhs_reg = self.lower_expr(*left, arena)?;
                let rhs_reg = self.lower_expr(*right, arena)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Pipe(dst, lhs_reg, rhs_reg));
                Ok(dst)
            }
            // α.1: p"..." 模板拼接（不触发 AI，只拼接字符串）
            ExprKind::Prompt { parts } => {
                let part_regs: Vec<Reg> = parts
                    .iter()
                    .map(|p| self.lower_expr(*p, arena))
                    .collect::<Result<_, _>>()?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Prompt(dst, part_regs));
                Ok(dst)
            }
            _ => Err(format!(
                "lower_expr: ExprKind {:?} not yet supported (α.1)",
                std::mem::discriminant(&expr.kind)
            )),
        }
    }

    // ── 语句 lowering ──

    fn lower_stmt(&mut self, sid: NodeId, arena: &AstArena) -> Result<(), String> {
        let stmt = arena
            .get_stmt(sid)
            .ok_or_else(|| format!("lower_stmt: NodeId {} not in arena", sid.0))?;
        match &stmt.kind {
            StmtKind::Let { name, init, .. } => {
                let r = self.lower_expr(*init, arena)?;
                self.emit(MirInst::Define(name.clone(), r));
                Ok(())
            }
            StmtKind::Assign { name, value } => {
                let r = self.lower_expr(*value, arena)?;
                self.emit(MirInst::Assign(name.clone(), r));
                Ok(())
            }
            StmtKind::Expr(eid) => {
                let r = self.lower_expr(*eid, arena)?;
                self.emit(MirInst::Expr(r));
                Ok(())
            }
            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let c = self.lower_expr(*condition, arena)?;
                // emit JumpIfNot with placeholder label; record its index for patching
                self.emit(MirInst::JumpIfNot(c, 0)); // placeholder 0
                let jumpifnot_idx = self.insts.len() - 1;
                for s in then_branch {
                    self.lower_stmt(*s, arena)?;
                }
                // emit Jump to end with placeholder; record its index for patching
                self.emit(MirInst::Jump(0)); // placeholder 0
                let jump_end_idx = self.insts.len() - 1;
                // else 分支起始 = 当前 insts.len()
                let else_start = self.insts.len();
                self.patch_label_at(jumpifnot_idx, else_start);
                for s in else_branch {
                    self.lower_stmt(*s, arena)?;
                }
                // end = 当前 insts.len()（else 分支之后）
                let end = self.insts.len();
                self.patch_label_at(jump_end_idx, end);
                // 注：α.0 不 emit Label 指令（label 即索引），JumpIfNot/Jump 直接用索引
                Ok(())
            }
            StmtKind::Return { value } => {
                match value {
                    Some(eid) => {
                        let r = self.lower_expr(*eid, arena)?;
                        self.emit(MirInst::Return(Some(r)));
                    }
                    None => self.emit(MirInst::Return(None)),
                }
                Ok(())
            }
            StmtKind::Break => {
                let (cont, brk) = self
                    .loop_stack
                    .last()
                    .copied()
                    .ok_or("Break outside loop")?;
                let _ = cont;
                self.emit(MirInst::Break(brk));
                Ok(())
            }
            StmtKind::Continue => {
                let (cont, brk) = self
                    .loop_stack
                    .last()
                    .copied()
                    .ok_or("Continue outside loop")?;
                let _ = brk;
                self.emit(MirInst::Continue(cont));
                Ok(())
            }
            // α.1: For 循环展开为索引循环
            StmtKind::For {
                var,
                iterable,
                body,
                ..
            } => {
                use crate::value::Value;
                // __iter_reg = lower(iterable)
                let iter_reg = self.lower_expr(*iterable, arena)?;
                // __i_reg = 0
                let i_reg = self.alloc_reg();
                self.emit(MirInst::Const(i_reg, Value::Int(0)));
                // __len_reg = len(__iter_reg)
                let len_reg = self.alloc_reg();
                self.emit(MirInst::Call(len_reg, "len".to_string(), vec![iter_reg]));
                // one_reg = 1（用于 i += 1）
                let one_reg = self.alloc_reg();
                self.emit(MirInst::Const(one_reg, Value::Int(1)));

                // loop_label: continue 跳回这里
                let loop_label = self.insts.len();
                // cond = i >= len
                let cond_reg = self.alloc_reg();
                self.emit(MirInst::BinaryOp(
                    cond_reg,
                    i_reg,
                    BinaryOp::GreaterEqual,
                    len_reg,
                ));
                // if cond: goto end（占位，稍后回填）
                self.emit(MirInst::JumpIf(cond_reg, 0));
                let exit_jump_idx = self.insts.len() - 1;

                // x = __iter_reg[__i_reg]
                let x_reg = self.alloc_reg();
                self.emit(MirInst::Index(x_reg, iter_reg, i_reg));
                self.emit(MirInst::Define(var.clone(), x_reg));

                // body lowering（Break/Continue emit 占位 0，稍后扫描回填）
                let body_start = self.insts.len();
                self.loop_stack.push((loop_label, 0)); // break label 占位 0
                for s in body {
                    self.lower_stmt(*s, arena)?;
                }
                self.loop_stack.pop();
                let body_end = self.insts.len();

                // incr: i = i + 1; goto loop
                self.emit(MirInst::BinaryOp(i_reg, i_reg, BinaryOp::Add, one_reg));
                self.emit(MirInst::Jump(loop_label));

                // end_label: break 跳到这里
                let end_label = self.insts.len();
                // 回填 exit jump → end_label
                self.patch_label_at(exit_jump_idx, end_label);
                // 扫描 body [body_start..body_end) 回填 Break/Continue 占位
                for i in body_start..body_end {
                    match &mut self.insts[i] {
                        MirInst::Break(lbl) => *lbl = end_label,
                        MirInst::Continue(lbl) => *lbl = loop_label,
                        _ => {}
                    }
                }
                Ok(())
            }
            // α.2: task 定义——递归 lower body 成嵌套 MirFunction
            StmtKind::TaskDef {
                name, params, body, ..
            } => {
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                // 递归 lower body（用新 Lowerer，独立寄存器空间）
                let mut body_lowerer = Lowerer::new();
                for s in body {
                    body_lowerer.lower_stmt(*s, arena)?;
                }
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::TaskDef {
                    name: name.clone(),
                    params: param_names,
                    body: Box::new(body_mir),
                });
                Ok(())
            }
            // α.2: import — 委托解释器处理
            StmtKind::Import { path } => {
                self.emit(MirInst::Import(path.clone()));
                Ok(())
            }
            // α.2: with 块 — 保存/恢复 AI config
            StmtKind::With { bindings, body } => {
                let binding_regs: Vec<(String, Reg)> = bindings
                    .iter()
                    .map(|(k, v)| self.lower_expr(*v, arena).map(|r| (k.clone(), r)))
                    .collect::<Result<_, _>>()?;
                let mut body_lowerer = Lowerer::new();
                for s in body {
                    body_lowerer.lower_stmt(*s, arena)?;
                }
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::WithConfig {
                    bindings: binding_regs,
                    body: Box::new(body_mir),
                });
                Ok(())
            }
            // α.2: parallel — AST 解释器也是顺序执行，MIR 直接展开
            StmtKind::Parallel { stmts } => {
                for s in stmts {
                    self.lower_stmt(*s, arena)?;
                }
                Ok(())
            }
            _ => Err(format!(
                "lower_stmt: StmtKind {:?} not yet supported (α.2)",
                std::mem::discriminant(&stmt.kind)
            )),
        }
    }

    /// 回填某条 JumpIfNot 指令的 label 为实际索引
    /// 回填指定索引处指令的 label 为实际值
    fn patch_label_at(&mut self, idx: usize, label: Label) {
        match &mut self.insts[idx] {
            MirInst::JumpIfNot(_, lbl) | MirInst::JumpIf(_, lbl) | MirInst::Jump(lbl) => {
                *lbl = label;
            }
            _ => {}
        }
    }
}
