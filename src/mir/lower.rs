//! AST → MIR lowering pass（α.0）
//!
//! 遍历 ASTv2，为每个表达式分配虚拟寄存器，生成线性 MIR 指令序列。
//! 控制流（If/For）展平为 Label + Jump。FlowSignal 枚举传返 → Jump/Return/Break/Continue 指令。

use crate::ast_v2::{AstArena, ExprKind, NodeId, Pattern, StmtKind};
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

/// α.11: 单表达式 lowering。返回一个含单个 Expr 指令序列的 MirFunction，
/// 末尾追加 Return(dst) 让 run_mir 返回表达式的值。
/// 用于从 orchestrator / trait_dispatch 等替代 self.evaluate(*node_id, arena)。
pub fn lower_expr_only(expr_id: NodeId, arena: &AstArena) -> Result<MirFunction, String> {
    let mut l = Lowerer::new();
    let dst = l.lower_expr(expr_id, arena)?;
    l.emit(MirInst::Return(Some(dst)));
    Ok(l.finish())
}

/// α.11: 单语句 lowering（用于 call_value_inner / call_task_inner 的 arena 替代）。
/// 末尾追加 Return 最后一个寄存器的值（如果 stmt 是 Expr）。
pub fn lower_stmt_only(stmt_id: NodeId, arena: &AstArena) -> Result<MirFunction, String> {
    let mut l = Lowerer::new();
    l.lower_stmt(stmt_id, arena)?;
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

    #[allow(unreachable_patterns)]
    fn lower_expr(&mut self, eid: NodeId, arena: &AstArena) -> Result<Reg, String> {
        let expr = arena
            .get_expr(eid)
            .ok_or_else(|| format!("lower_expr: NodeId {} not in arena", eid.0))?;
        match &expr.kind {
            MirExpr::Literal(lit) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, literal_to_value_static(lit)));
                Ok(dst)
            }
            MirExpr::Variable(name) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Var(dst, name.clone()));
                Ok(dst)
            }
            MirExpr::Binary { left, op, right } => {
                let l = self.lower_expr(*left, arena)?;
                let r = self.lower_expr(*right, arena)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::BinaryOp(dst, l, op.clone(), r));
                Ok(dst)
            }
            MirExpr::Call { callee, args } => {
                let arg_regs: Vec<Reg> = args
                    .iter()
                    .map(|a| self.lower_expr(*a, arena))
                    .collect::<Result<_, _>>()?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Call(dst, callee.clone(), arg_regs));
                Ok(dst)
            }
            MirExpr::Grouping(inner) => {
                // 分组表达式：透传内部表达式
                self.lower_expr(*inner, arena)
            }
            // α.1: 列表字面量
            MirExpr::List(items) => {
                let item_regs: Vec<Reg> = items
                    .iter()
                    .map(|i| self.lower_expr(*i, arena))
                    .collect::<Result<_, _>>()?;
                let dst = self.alloc_reg();
                self.emit(MirInst::ListLit(dst, item_regs));
                Ok(dst)
            }
            // α.1: 字典字面量（key 是 String，val 是 NodeId）
            MirExpr::Dict(pairs) => {
                let pair_regs: Vec<(String, Reg)> = pairs
                    .iter()
                    .map(|(k, v)| self.lower_expr(*v, arena).map(|r| (k.clone(), r)))
                    .collect::<Result<_, _>>()?;
                let dst = self.alloc_reg();
                self.emit(MirInst::DictLit(dst, pair_regs));
                Ok(dst)
            }
            // α.1: 索引 obj[idx]
            MirExpr::Index { object, index } => {
                let obj_reg = self.lower_expr(*object, arena)?;
                let idx_reg = self.lower_expr(*index, arena)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Index(dst, obj_reg, idx_reg));
                Ok(dst)
            }
            // α.1: 方法调用 recv.method(args)
            MirExpr::MethodCall {
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
            MirExpr::Pipe { left, right } => {
                let lhs_reg = self.lower_expr(*left, arena)?;
                let rhs_reg = self.lower_expr(*right, arena)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Pipe(dst, lhs_reg, rhs_reg));
                Ok(dst)
            }
            // α.1: p"..." 模板拼接（不触发 AI，只拼接字符串）
            MirExpr::Prompt { parts } => {
                let part_regs: Vec<Reg> = parts
                    .iter()
                    .map(|p| self.lower_expr(*p, arena))
                    .collect::<Result<_, _>>()?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Prompt(dst, part_regs));
                Ok(dst)
            }
            // α.2: 模式匹配表达式——lowering 为 MatchExpr 指令
            MirExpr::Match { expr, arms } => {
                let val_reg = self.lower_expr(*expr, arena)?;
                let match_arms: Vec<(String, Option<Reg>, Box<MirFunction>, Reg)> = arms
                    .iter()
                    .map(|(pattern, arm_eid)| {
                        let pat_str = self.pattern_to_string(pattern);
                        let mut body_lowerer = Lowerer::new();
                        let arm_val_reg = body_lowerer.lower_expr(*arm_eid, arena)?;
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
            // α.10: 闭包字面量 — 递归 lower body 成嵌套 MirFunction（独立寄存器空间）。
            // 模板与 TaskDef 一致：return_type 字段被丢弃（typeck 独立处理）。
            MirExpr::Closure {
                params,
                return_type: _,
                body,
            } => {
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                let mut body_lowerer = Lowerer::new();
                for sid in body {
                    body_lowerer.lower_stmt(*sid, arena)?;
                }
                let body_mir = body_lowerer.finish();
                let dst = self.alloc_reg();
                self.emit(MirInst::Closure {
                    dst,
                    params: param_names,
                    body: Box::new(body_mir),
                });
                Ok(dst)
            }
            // α.12: `expr as dyn Trait` → MirInst::DynTrait
            MirExpr::DynTrait {
                expr,
                trait_generics,
                trait_name,
            } => {
                let src = self.lower_expr(*expr, arena)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::DynTrait {
                    dst,
                    src,
                    trait_generics: trait_generics.clone(),
                    trait_name: trait_name.clone(),
                });
                Ok(dst)
            }
            _ => Err(format!(
                "lower_expr: ExprKind {:?} not yet supported (α.10)",
                std::mem::discriminant(&expr.kind)
            )),
        }
    }

    // ── 语句 lowering ──

    #[allow(unreachable_patterns)]
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
            StmtKind::With {
                bindings,
                body,
                jit,
            } => {
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
                    jit: *jit,
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
            // α.2: index assignment — obj[idx] = val
            StmtKind::IndexAssign {
                object,
                index,
                value,
            } => {
                let obj_reg = self.lower_expr(*object, arena)?;
                let idx_reg = self.lower_expr(*index, arena)?;
                let val_reg = self.lower_expr(*value, arena)?;
                self.emit(MirInst::IndexAssign(obj_reg, idx_reg, val_reg));
                Ok(())
            }
            // α.2: match 语句 — lowering 为 MatchExpr
            // 注意：parser 将 match statement 的 arm body 存储为
            // Vec<NodeId>，但实际内容是表达式而非语句，需用 lower_expr。
            StmtKind::Match { expr, arms } => {
                let val_reg = self.lower_expr(*expr, arena)?;
                let match_arms: Vec<(String, Option<Reg>, Box<MirFunction>, Reg)> = arms
                    .iter()
                    .map(|(pattern, body_ids)| {
                        let pat_str = self.pattern_to_string(pattern);
                        // body_ids 中存储的是表达式，需要 lower_expr
                        let mut body_lowerer = Lowerer::new();
                        if let Some(&arm_eid) = body_ids.first() {
                            let arm_val_reg = body_lowerer.lower_expr(arm_eid, arena)?;
                            body_lowerer.emit(MirInst::Return(Some(arm_val_reg)));
                        }
                        Ok((pat_str, None, Box::new(body_lowerer.finish()), 0))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                let _dst = self.alloc_reg();
                self.emit(MirInst::MatchExpr {
                    val: val_reg,
                    arms: match_arms,
                });
                Ok(())
            }
            // α.2: stream_for — stream_for var in prompt body end
            StmtKind::StreamFor { prompt, var, body } => {
                let prompt_reg = self.lower_expr(*prompt, arena)?;
                let mut body_lowerer = Lowerer::new();
                for s in body {
                    body_lowerer.lower_stmt(*s, arena)?;
                }
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::StreamFor {
                    prompt_reg,
                    var: var.clone(),
                    body: Box::new(body_mir),
                });
                Ok(())
            }
            // α.2: tool 定义
            StmtKind::ToolDef {
                name,
                description,
                params,
                return_type,
                body,
                exported,
            } => {
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                let return_type_str = return_type.clone();
                let mut body_lowerer = Lowerer::new();
                for s in body {
                    body_lowerer.lower_stmt(*s, arena)?;
                }
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::ToolDef {
                    name: name.clone(),
                    description: description.clone(),
                    params: param_names,
                    return_type: return_type_str,
                    body: Box::new(body_mir),
                    exported: *exported,
                });
                Ok(())
            }
            // α.3: 类型别名
            StmtKind::TypeAlias { name, target, .. } => {
                self.emit(MirInst::TypeAlias {
                    name: name.clone(),
                    target: target.clone(),
                });
                Ok(())
            }
            // α.3: 枚举定义
            StmtKind::EnumDef { name, variants, .. } => {
                self.emit(MirInst::EnumDef {
                    name: name.clone(),
                    variants: variants.clone(),
                });
                Ok(())
            }
            // α.3: 结构体定义
            StmtKind::StructDef { name, fields, .. } => {
                self.emit(MirInst::StructDef {
                    name: name.clone(),
                    fields: fields.clone(),
                });
                Ok(())
            }
            // α.4: 事务 — body + compensation 分别 lower 成嵌套 MirFunction
            StmtKind::Transaction { body, compensation } => {
                let mut body_lowerer = Lowerer::new();
                for s in body {
                    body_lowerer.lower_stmt(*s, arena)?;
                }
                let body_mir = body_lowerer.finish();
                let mut comp_lowerer = Lowerer::new();
                for s in compensation {
                    comp_lowerer.lower_stmt(*s, arena)?;
                }
                let comp_mir = comp_lowerer.finish();
                self.emit(MirInst::Transaction {
                    body: Box::new(body_mir),
                    compensation: Box::new(comp_mir),
                });
                Ok(())
            }
            // α.4: send — 求值 value 为寄存器，emit Send 指令
            StmtKind::Send { value, target } => {
                let val_reg = self.lower_expr(*value, arena)?;
                self.emit(MirInst::Send {
                    value: val_reg,
                    target: target.clone(),
                });
                Ok(())
            }
            // α.4: receive — emit Receive 指令（var/source 直接传递）
            StmtKind::Receive { var, source } => {
                self.emit(MirInst::Receive {
                    var: var.clone(),
                    source: source.clone(),
                });
                Ok(())
            }
            // α.4: rollback — 返回事务回滚错误
            StmtKind::Rollback => {
                self.emit(MirInst::Rollback);
                Ok(())
            }
            // α.5: macro def — 注册宏到环境（body 内容不 lower，宏在运行期展开）
            StmtKind::MacroDef {
                name,
                params,
                body: _,
            } => {
                self.emit(MirInst::MacroDef {
                    name: name.clone(),
                    params: params.clone(),
                });
                Ok(())
            }
            // α.5: commit — no-op
            StmtKind::Commit => Ok(()),
            // α.5: worker — 并发 worker（顺序执行 body）
            StmtKind::Worker { name, body } => {
                let mut body_lowerer = Lowerer::new();
                for s in body {
                    body_lowerer.lower_stmt(*s, arena)?;
                }
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::Worker {
                    name: name.clone(),
                    body: Box::new(body_mir),
                });
                Ok(())
            }
            // α.5: route — 路由声明（不实现，解释器返回错误）
            StmtKind::Route { name, target: _ } => {
                self.emit(MirInst::Route(name.clone()));
                Ok(())
            }
            // α.5: observe — 可观测性块（执行 body，配置记录但不副作用）
            StmtKind::Observe { config, body } => {
                let config_str = match config {
                    crate::ast_v2::ObserveConfig::Trace => "trace".to_string(),
                    crate::ast_v2::ObserveConfig::Metrics => "metrics".to_string(),
                    crate::ast_v2::ObserveConfig::Otel { endpoint: _ } => "otel".to_string(),
                };
                let mut body_lowerer = Lowerer::new();
                for s in body {
                    body_lowerer.lower_stmt(*s, arena)?;
                }
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::Observe {
                    config: config_str,
                    body: Box::new(body_mir),
                });
                Ok(())
            }
            // α.5: span — 追踪 span（执行 body，name 记录但不执行实际追踪）
            StmtKind::Span {
                name,
                attributes: _,
                body,
            } => {
                let mut body_lowerer = Lowerer::new();
                for s in body {
                    body_lowerer.lower_stmt(*s, arena)?;
                }
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::Span {
                    name: name.clone(),
                    body: Box::new(body_mir),
                });
                Ok(())
            }
            // α.5: record_tokens — 记录 token 输入输出（no-op）
            StmtKind::RecordTokens { input, output } => {
                let input_str = format!("{}", input.0);
                let output_str = format!("{}", output.0);
                self.emit(MirInst::RecordTokens {
                    input: input_str,
                    output: output_str,
                });
                Ok(())
            }
            // α.6: save — 将 value 序列化为文件
            StmtKind::Save { path, value } => {
                let path_reg = self.lower_expr(*path, arena)?;
                let val_reg = self.lower_expr(*value, arena)?;
                self.emit(MirInst::Save {
                    path: path_reg,
                    value: val_reg,
                });
                Ok(())
            }
            // α.6: load — 从文件加载 JSON 值
            StmtKind::Load { path, var } => {
                let path_reg = self.lower_expr(*path, arena)?;
                self.emit(MirInst::Load {
                    path: path_reg,
                    var: var.clone(),
                });
                Ok(())
            }
            // α.6: read_file — 读取文件为字符串
            StmtKind::ReadFile { path, var } => {
                let path_reg = self.lower_expr(*path, arena)?;
                self.emit(MirInst::ReadFile {
                    path: path_reg,
                    var: var.clone(),
                });
                Ok(())
            }
            // α.6: write_file — 将 content 写入文件
            StmtKind::WriteFile { path, content } => {
                let path_reg = self.lower_expr(*path, arena)?;
                let content_reg = self.lower_expr(*content, arena)?;
                self.emit(MirInst::WriteFile {
                    path: path_reg,
                    content: content_reg,
                });
                Ok(())
            }
            // α.6: append_file — 将 content 追加到文件
            StmtKind::AppendFile { path, content } => {
                let path_reg = self.lower_expr(*path, arena)?;
                let content_reg = self.lower_expr(*content, arena)?;
                self.emit(MirInst::AppendFile {
                    path: path_reg,
                    content: content_reg,
                });
                Ok(())
            }
            // α.6: read_bytes_file — 读取文件为字节数组
            StmtKind::ReadBytesFile { path, var } => {
                let path_reg = self.lower_expr(*path, arena)?;
                self.emit(MirInst::ReadBytesFile {
                    path: path_reg,
                    var: var.clone(),
                });
                Ok(())
            }
            // α.6: write_bytes_file — 将 hex 字节写入文件
            StmtKind::WriteBytesFile { path, content } => {
                let path_reg = self.lower_expr(*path, arena)?;
                let content_reg = self.lower_expr(*content, arena)?;
                self.emit(MirInst::WriteBytesFile {
                    path: path_reg,
                    content: content_reg,
                });
                Ok(())
            }
            // α.7: trait def — 注册 trait 到 trait_registry + 默认实现
            StmtKind::TraitDef {
                name,
                generics: _,
                parents,
                methods,
                trait_where: _,
            } => {
                // α.11: prelower 每个 method body 成 MirFunction（与 methods 并行）。
                let method_bodies: Vec<crate::mir::MirFunction> = methods
                    .iter()
                    .map(|m| {
                        let mut l = Lowerer::new();
                        for sid in &m.body {
                            l.lower_stmt(*sid, arena)?;
                        }
                        Ok::<_, String>(l.finish())
                    })
                    .collect::<Result<_, _>>()?;
                self.emit(MirInst::TraitDef {
                    name: name.clone(),
                    parents: parents.clone(),
                    methods: methods.clone(),
                    method_bodies,
                });
                Ok(())
            }
            // α.7: impl def — 注册 impl 到 impl_table + 方法到环境
            StmtKind::ImplDef {
                generics: _,
                trait_generics,
                trait_name,
                for_type,
                for_generics,
                where_clause: _,
                methods,
            } => {
                // α.11: prelower method bodies。
                let method_bodies: Vec<crate::mir::MirFunction> = methods
                    .iter()
                    .map(|m| {
                        let mut l = Lowerer::new();
                        for sid in &m.body {
                            l.lower_stmt(*sid, arena)?;
                        }
                        Ok::<_, String>(l.finish())
                    })
                    .collect::<Result<_, _>>()?;
                self.emit(MirInst::ImplDef {
                    trait_name: trait_name.clone(),
                    trait_generics: trait_generics.clone(),
                    for_type: for_type.clone(),
                    for_generics: for_generics.clone(),
                    methods: methods.clone(),
                    method_bodies,
                });
                Ok(())
            }
            // α.8: orchestrate — 编排执行
            StmtKind::Orchestrate {
                input_var,
                result_var,
                kind,
            } => {
                use crate::ast_v2::OrchestrateKind;
                let kind: Box<crate::mir::expr::MirOrchestrateKind> = Box::new(match kind {
                    OrchestrateKind::Sequential { .. } => crate::mir::expr::MirOrchestrateKind::Sequential { agents: vec![] },
                    OrchestrateKind::Graph { .. } => crate::mir::expr::MirOrchestrateKind::Graph { agents: vec![], edges: vec![] },
                    OrchestrateKind::Loop { .. } => crate::mir::expr::MirOrchestrateKind::Loop { agents: vec![], rounds: None, exit_when: None },
                    OrchestrateKind::Pregel { .. } => crate::mir::expr::MirOrchestrateKind::Pregel {
                        agents: vec![],
                        edges: vec![],
                        state_schema: vec![],
                        checkpoint: None,
                        interrupt_points: vec![],
                        adjacency: std::collections::HashMap::new(),
                    },
                });
                self.emit(MirInst::Orchestrate {
                    input_var: input_var.clone(),
                    result_var: result_var.clone(),
                    kind,
                });
                Ok(())
            }
            // α.8: eval — 断言测试
            StmtKind::Eval {
                name,
                given,
                expects,
                tolerance,
                replay_path,
            } => {
                let given_reg = self.lower_expr(*given, arena)?;
                let expect_regs: Vec<Reg> = expects
                    .iter()
                    .map(|e| self.lower_expr(*e, arena))
                    .collect::<Result<_, _>>()?;
                self.emit(MirInst::Eval {
                    name: name.clone(),
                    given_reg,
                    expects: expect_regs,
                    tolerance: *tolerance,
                    replay_path: replay_path.clone(),
                });
                Ok(())
            }
            // α.8: skill def — 构建 Skill Dict 到环境
            StmtKind::SkillDef {
                name,
                description,
                version,
                requires,
                tasks,
                verify,
            } => {
                // α.11: prelower 每个 task body。
                let task_bodies: Vec<crate::mir::MirFunction> = tasks
                    .iter()
                    .map(|t| {
                        let mut l = Lowerer::new();
                        for sid in &t.body {
                            l.lower_stmt(*sid, arena)?;
                        }
                        Ok::<_, String>(l.finish())
                    })
                    .collect::<Result<_, _>>()?;
                // α.11: prelower verify body。
                let verify_body = if let Some(v) = verify {
                    let mut l = Lowerer::new();
                    for sid in &v.body {
                        l.lower_stmt(*sid, arena)?;
                    }
                    Some(l.finish())
                } else {
                    None
                };
                self.emit(MirInst::SkillDef {
                    name: name.clone(),
                    description: description.clone(),
                    version: version.clone(),
                    requires: requires.clone(),
                    tasks: tasks.clone(),
                    task_bodies,
                    verify: verify.clone(),
                    verify_body,
                });
                Ok(())
            }
            // α.8: prompt section — 扫描 body 构建 PromptSection
            StmtKind::PromptSection { name, body } => {
                let mut body_lowerer = Lowerer::new();
                for s in body {
                    body_lowerer.lower_stmt(*s, arena)?;
                }
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::PromptSection {
                    name: name.clone(),
                    body: Box::new(body_mir),
                });
                Ok(())
            }
            // α.8: prompt set / prompt read — 在 prompt section 内部，不直接出现在顶层
            // （这些由 prompt section 内部处理，顶层出现时当作 no-op）
            StmtKind::PromptSet { key: _, value: _ } => Ok(()),
            StmtKind::PromptRead(_path) => Ok(()),
            // α.8: document section — 扫描 body 构建 DocumentSection
            StmtKind::DocumentSection { name, body } => {
                let mut body_lowerer = Lowerer::new();
                for s in body {
                    body_lowerer.lower_stmt(*s, arena)?;
                }
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::DocumentSection {
                    name: name.clone(),
                    body: Box::new(body_mir),
                });
                Ok(())
            }
            // α.8: document set / document read — 在 document section 内部处理
            StmtKind::DocumentSet { key: _, value: _ } => Ok(()),
            StmtKind::DocumentRead(_path) => Ok(()),
            // α.9: 所有 StmtKind 变体均已被覆盖，此处不应出现
            other => {
                use std::mem;
                // 用 _ 模式静默匹配任何遗漏变体（如果将来新增）
                // 但编译期确保 unreachable
                Err(format!(
                    "lower_stmt: unexpected StmtKind variant: {:?}",
                    mem::discriminant(other)
                ))
            }
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

    /// 将 Pattern 转换为字符串描述，用于 MIR MatchExpr
    /// MIR 解释器根据模式类型执行不同的匹配逻辑
    fn pattern_to_string(&self, pattern: &Pattern) -> String {
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
            Pattern::List { prefix, rest } => {
                let prefixes: Vec<String> =
                    prefix.iter().map(|p| self.pattern_to_string(p)).collect();
                if let Some(r) = rest {
                    format!("list:[{}]|rest:{}", prefixes.join(","), r)
                } else {
                    format!("list:[{}]", prefixes.join(","))
                }
            }
            Pattern::Dict(entries) => {
                let fields: Vec<String> = entries
                    .iter()
                    .map(|(k, v)| format!("{}:{}", k, self.pattern_to_string(v)))
                    .collect();
                format!("dict:{{{}}}", fields.join(","))
            }
            Pattern::Guard {
                pattern,
                condition: _,
            } => {
                format!("guard:{}", self.pattern_to_string(pattern))
            }
        }
    }
}

// ── MirExpr-based lowering (v0.55: V3 pipeline) ──

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
    Ok(l.finish())
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
        use crate::common::Literal;

        match &expr.kind {
        // ── Literals ──
        MirExpr::Literal(crate::common::Literal::Int(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Int(*v)));
                Ok(dst)
            }
            MirExpr::Literal(crate::common::Literal::Float(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Float(*v)));
                Ok(dst)
            }
                        MirExpr::Literal(crate::common::Literal::String(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::String(v.clone())));
                Ok(dst)
            }
                        MirExpr::Literal(crate::common::Literal::Bool(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Bool(*v)));
                Ok(dst)
            }
                        MirExpr::Literal(crate::common::Literal::Char(c, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Char(*c)));
                Ok(dst)
            }
                        MirExpr::Literal(crate::common::Literal::Nil(_)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Variables ──
                        MirExpr::Variable(name) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Var(dst, name.clone()));
                Ok(dst)
            }

            // ── Binary operations ──
                        MirExpr::Binary { left, op, right } => {
                let l = self.lower_expr(left)?;
                let r = self.lower_expr(right)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::BinaryOp(dst, l, op.clone(), r));
                Ok(dst)
            }

            // ── Logical And/Or (short-circuit) ──
                        MirExpr::And { left, right } => {
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
                        MirExpr::Or { left, right } => {
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
                        MirExpr::Call { callee, args } => {
                // TODO: Implement proper callee extraction during migration
                let callee_name = "call".to_string();
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
                        MirExpr::MethodCall { receiver, method, args } => {
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
                        MirExpr::Pipe { lhs, rhs } => {
                let lhs_reg = self.lower_expr(lhs)?;
                let rhs_reg = self.lower_expr(rhs)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Pipe(dst, lhs_reg, rhs_reg));
                Ok(dst)
            }

            // ── Collections ──
                        MirExpr::List(items) => {
                let mut item_regs: Vec<Reg> = Vec::new();
                for item in items {
                    let r = self.lower_expr(item)?;
                    item_regs.push(r);
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::ListLit(dst, item_regs));
                Ok(dst)
            }
                        MirExpr::Dict(entries) => {
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
                        MirExpr::If { cond, then_branch, else_branch } => {
                let c = self.lower_expr(cond)?;
                // JumpIfNot to else branch
                self.emit(MirInst::JumpIfNot(c, 0)); // placeholder
                let jumpifnot_idx = self.insts.len() - 1;

                // Then branch
                let then_dst = self.lower_expr(then_branch)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Assign("__if_result".to_string(), then_dst));
                // Jump to end
                self.emit(MirInst::Jump(0)); // placeholder
                let jump_end_idx = self.insts.len() - 1;

                // Else branch
                let else_start = self.insts.len();
                self.patch_label_at(jumpifnot_idx, else_start);
                if let Some(else_expr) = else_branch {
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
            MirExpr::Match { scrutinee, arms } => {
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
            MirExpr::Loop { var, iterable, body } => {
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
            MirExpr::While { cond, body } => {
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
                        MirExpr::Closure { params, body } => {
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
            MirExpr::FnDef { name, params, body, .. } => {
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
            MirExpr::DynTrait { expr, trait_name, generics } => {
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
            MirExpr::Prompt { parts } => {
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
            MirExpr::LetBinding { name, value, init_body, .. } => {
                let v_dst = self.lower_expr(value)?;
                self.emit(MirInst::Define(name.clone(), v_dst));
                let b_dst = self.lower_expr(init_body)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Assign("__let_result".to_string(), b_dst));
                self.emit(MirInst::Var(dst, "__let_result".to_string()));
                Ok(dst)
            }

            // ── Assignment ──
            MirExpr::Assign { target, value } => {
                let v = self.lower_expr(value)?;
                self.emit(MirInst::Assign(target.clone(), v));
                Ok(v)
            }

            // ── IndexAssign ──
            MirExpr::IndexAssign { object, index, value } => {
                let obj = self.lower_expr(object)?;
                let idx = self.lower_expr(index)?;
                let val = self.lower_expr(value)?;
                self.emit(MirInst::IndexAssign(obj, idx, val));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Expr (discard result) ──
            MirExpr::Expr(inner) => {
                let r = self.lower_expr(inner)?;
                self.emit(MirInst::Expr(r));
                Ok(r)
            }

            // ── Sequence ──
            MirExpr::Sequence(exprs) => {
                let mut last_dst = 0;
                for e in exprs {
                    last_dst = self.lower_expr(e)?;
                }
                Ok(last_dst)
            }

            // ── Return / Break / Continue ──
            MirExpr::Return(val) => {
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
            MirExpr::Break(_label) => {
                let (_, brk) = self.loop_stack.last().copied().ok_or("Break outside loop")?;
                self.emit(MirInst::Break(brk));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            MirExpr::Continue(_label) => {
                let (cont, _) = self.loop_stack.last().copied().ok_or("Continue outside loop")?;
                self.emit(MirInst::Continue(cont));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Orchestrate ──
            MirExpr::Orchestrate { input_var, result_var, kind } => {
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
            MirExpr::TypeAlias { name, target } => {
                self.emit(MirInst::TypeAlias {
                    name: name.clone(),
                    target: target.name(),
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            MirExpr::EnumDef { name, variants } => {
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
            MirExpr::StructDef { name, fields } => {
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
            MirExpr::Import(path) => {
                self.emit(MirInst::Import(path.clone()));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            MirExpr::MacroDef { name, params } => {
                self.emit(MirInst::MacroDef {
                    name: name.clone(),
                    params: params.clone(),
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Grouping (transparent) ──
            MirExpr::Grouping(inner) => {
                self.lower_expr(inner)
            }
        }
    }
}

/// Convert MirExpr Pattern to string representation for MatchExpr
fn mir_pattern_to_string(pattern: &crate::mir::expr::Pattern) -> String {
    match pattern {
        crate::mir::expr::Pattern::Wildcard => "_".to_string(),
        crate::mir::expr::Pattern::Variable(name) => name.clone(),
    }
}
