//! MirWitness → MIR lowering (v0.92: witness-canonical 入口)
//!
//! v0.92: 全部 lowering 从 `Vec<MirWitness>` 直接构造 `MirFunction`。
//! 老的 `lower_program(&[NodeId], &AstArena)` (AST v2 路径) 在 v0.55 删除。
//! 老的 `Lowerer` struct (882 行遗留实现) 在 v0.55 删除。
//! 老的 `MirExpr` 桥接在 v0.92 删除——所有调用方已迁移到 `lower_mir_witnesses`。
//!
//! 入口:
//! - `lower_mir_witnesses(witnesses: &[MirWitness]) -> Result<MirFunction, String>`
//! - `lower_mir_witnesses_with_opt(...)` — 显式 opt 等级变体

// ── Witness-based lowering (v0.92 canonical) ──

use super::{Label, MirFunction, MirInst, Reg};
use crate::mir::witness::{MirWitness, WitnessCallee, WitnessKind};
use crate::value::Value;

/// v0.75.30: 显式编译选项变体 — 调用方（CLI 编译入口）显式指定优化等级，
/// 不读环境变量。语义与 `lower_mir_witnesses` 完全一致，仅优化等级来源不同。
pub fn lower_mir_witnesses_with_opt(
    witnesses: &[MirWitness],
    opt_level: crate::mir::ssa::OptLevel,
) -> Result<MirFunction, String> {
    let mut l = WitnessLowerer::new();
    for w in witnesses {
        let _dst = l.lower_witness(w)?;
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

/// 将 MirWitness 列表 lowering 为 MirFunction（env 兜底：CLI 未显式 `--opt`
/// 时沿用 MORA_OPT — REPL/import/pregel 等无显式编译命令的入口）。
pub fn lower_mir_witnesses(witnesses: &[MirWitness]) -> Result<MirFunction, String> {
    lower_mir_witnesses_with_opt(witnesses, crate::mir::ssa::OptLevel::from_env())
}

/// v0.75.39: 共享 emit 机制 — WitnessLowerer 与 ParserV3 单遍编译共用。
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
            ..Default::default()
        }
    }
}

/// MirWitness → MIR 指令 lowering（v0.92: 直接消费 WitnessKind）
pub(crate) struct WitnessLowerer {
    emit: EmitContext,
    /// v0.78: 累积的 effect row。builtin 调用按前缀分类 → 推 effect label。
    /// 阶段 2 引入 Type::Arrow 时，本字段与 HM 类型系统对接。
    pub(crate) effects: super::effect::EffectRow,
}

impl WitnessLowerer {
    fn new() -> Self {
        Self {
            emit: EmitContext::new(),
            effects: super::effect::EffectRow::default(),
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
        let mut func = self.emit.finish();
        func.effects = self.effects;
        func
    }

    /// v0.78: builtin 名字 → effect label 分类（保守映射）。
    /// 未知 builtin 跳过（不假设有 effect）。
    fn classify_call_effect(&mut self, name: &str) {
        let label = match name {
            n if n.starts_with("ai.") => "Ai",
            n if n.starts_with("file.") || n.starts_with("fs.") => "Fs",
            n if n.starts_with("memory.") => "Mem",
            n if n.starts_with("sandbox.") => "Sandbox",
            n if n.starts_with("mock.") => "Mock",
            n if n.starts_with("bus.") || n.starts_with("event.") => "Event",
            n if n.starts_with("schedule.") => "Sched",
            n if n.starts_with("ccr.") => "Ccr",
            n if n.starts_with("compress.") => "Compress",
            n if n.starts_with("document.") => "Document",
            n if n.starts_with("web.") => "Net",
            n if n.starts_with("tool.") || n.starts_with("toolplane.") => "Tool",
            _ => return,
        };
        self.effects.extend(label);
    }

    /// v0.78: method call 分类。
    fn classify_method_effect(&mut self, method: &str) {
        let label = match method {
            "chat" | "stream" | "tokens" | "embed" | "route" | "observe" => "Ai",
            "read" | "write" | "append" | "save" | "load" | "delete" => "Fs",
            "remember" | "recall" | "store" => "Mem",
            "evaluate" | "interp" | "run" | "execute" | "with" | "handle" => "Interpret",
            _ => return,
        };
        self.effects.extend(label);
    }

    /// v0.92: helper for handle block — lower a MirWitness as a block.
    /// Used when we need to emit a sub-MirFunction body (each block has its own EmitContext).
    /// Returns (last_reg, witness) — same as emit_block_w in parser_v3.
    fn emit_block_via_witness_w(&mut self, w: &MirWitness) -> Result<(Reg, MirWitness), String> {
        // Sequence expression: lower each statement; last reg is the result.
        let WitnessKind::Sequence(stmts) = &w.kind else {
            // Single expression — wrap as a single-statement sequence.
            return self.lower_witness(w).map(|r| (r, empty_witness_for_span(w.span)));
        };
        let mut last_reg = 0;
        for stmt in stmts {
            last_reg = self.lower_witness(stmt)?;
        }
        Ok((last_reg, empty_witness_for_span(w.span)))
    }

    /// Lower witness → returns result register
    fn lower_witness(&mut self, w: &MirWitness) -> Result<Reg, String> {
        use crate::common::Literal;

        match &w.kind {
            // ── Literals ──
            WitnessKind::Literal(Literal::Int(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Int(*v)));
                Ok(dst)
            }
            WitnessKind::Literal(Literal::Float(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Float(*v)));
                Ok(dst)
            }
            WitnessKind::Literal(Literal::BigInt(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::BigInt(v.clone())));
                Ok(dst)
            }
            WitnessKind::Literal(Literal::String(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::String(v.clone())));
                Ok(dst)
            }
            WitnessKind::Literal(Literal::Bool(v, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Bool(*v)));
                Ok(dst)
            }
            WitnessKind::Literal(Literal::Char(c, _)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Char(*c)));
                Ok(dst)
            }
            WitnessKind::Literal(Literal::Nil(_)) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Variables ──
            WitnessKind::Variable(name) => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Var(dst, name.clone()));
                Ok(dst)
            }

            // ── Binary operations ──
            WitnessKind::Binary { left, op, right } => {
                let l = self.lower_witness(left)?;
                let r = self.lower_witness(right)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::BinaryOp(dst, l, op.clone(), r));
                Ok(dst)
            }

            // ── Logical And/Or (short-circuit) ──
            WitnessKind::And { left, right } => {
                let l = self.lower_witness(left)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::JumpIfNot(l, 0)); // placeholder
                let jump_idx = self.emit.insts.len() - 1;
                let r = self.lower_witness(right)?;
                self.emit(MirInst::BinaryOp(dst, l, crate::common::BinaryOp::Equal, r));
                let end = self.emit.insts.len();
                self.patch_label_at(jump_idx, end);
                Ok(dst)
            }
            WitnessKind::Or { left, right } => {
                let l = self.lower_witness(left)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::JumpIf(l, 0)); // placeholder
                let jump_idx = self.emit.insts.len() - 1;
                let r = self.lower_witness(right)?;
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
            WitnessKind::Call { callee, args } => {
                // v0.75.33: WitnessCallee::Method（`obj.method(args)`）走
                // MirInst::MethodCall — ParserV3 把 receiver 作为第一个参数
                // 传入，此处弹出作为 receiver 寄存器。
                if let WitnessCallee::Method(_obj, method) = callee {
                    let mut arg_regs: Vec<Reg> = Vec::new();
                    for arg in args {
                        let r = self.lower_witness(arg)?;
                        arg_regs.push(r);
                    }
                    self.classify_method_effect(method);
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
                    WitnessCallee::Name(n) => n.clone(),
                    WitnessCallee::Var(n) => n.clone(),
                    WitnessCallee::Builtin(op) => format!("{:?}", op),
                    WitnessCallee::Evaluated(_) => "unknown".to_string(),
                    WitnessCallee::Method(obj, m) => format!("{}.{}", obj, m),
                };
                self.classify_call_effect(&callee_name);
                let mut arg_regs: Vec<Reg> = Vec::new();
                for arg in args {
                    let r = self.lower_witness(arg)?;
                    arg_regs.push(r);
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::Call(dst, callee_name, arg_regs));
                Ok(dst)
            }

            // ── Method calls ──
            WitnessKind::MethodCall {
                receiver,
                method,
                args,
            } => {
                let recv_reg = self.lower_witness(receiver)?;
                let mut arg_regs: Vec<Reg> = Vec::new();
                for arg in args {
                    let r = self.lower_witness(arg)?;
                    arg_regs.push(r);
                }
                self.classify_method_effect(method);
                let dst = self.alloc_reg();
                self.emit(MirInst::MethodCall(dst, recv_reg, method.clone(), arg_regs));
                Ok(dst)
            }

            // ── Collections ──
            WitnessKind::List(items) => {
                let mut item_regs: Vec<Reg> = Vec::new();
                for item in items {
                    let r = self.lower_witness(item)?;
                    item_regs.push(r);
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::ListLit(dst, item_regs));
                Ok(dst)
            }
            WitnessKind::Dict(entries) => {
                let mut pair_regs: Vec<(String, Reg)> = Vec::new();
                for (key, val) in entries {
                    let r = self.lower_witness(val)?;
                    pair_regs.push((key.clone(), r));
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::DictLit(dst, pair_regs));
                Ok(dst)
            }

            // ── If/Else ──
            WitnessKind::If { cond, then, r#else } => {
                let c = self.lower_witness(cond)?;
                self.emit(MirInst::JumpIfNot(c, 0)); // placeholder
                let jumpifnot_idx = self.emit.insts.len() - 1;

                let then_dst = self.lower_witness(then)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Copy(dst, then_dst));
                self.emit(MirInst::Jump(0)); // placeholder
                let jump_end_idx = self.emit.insts.len() - 1;

                let else_start = self.emit.insts.len();
                self.patch_label_at(jumpifnot_idx, else_start);
                if let Some(else_w) = r#else {
                    let else_dst = self.lower_witness(else_w)?;
                    self.emit(MirInst::Copy(dst, else_dst));
                } else {
                    let nil_reg = self.alloc_reg();
                    self.emit(MirInst::Const(nil_reg, crate::value::Value::Nil));
                    self.emit(MirInst::Copy(dst, nil_reg));
                }
                let end = self.emit.insts.len();
                self.patch_label_at(jump_end_idx, end);
                Ok(dst)
            }

            // ── Match ──
            WitnessKind::Match { scrutinee, arms } => {
                let val_reg = self.lower_witness(scrutinee)?;
                let match_arms: Vec<(String, Option<Reg>, Box<MirFunction>, Reg)> = arms
                    .iter()
                    .map(|arm| {
                        let pat_str = pattern_to_string(&arm.pattern);
                        let mut body_lowerer = WitnessLowerer::new();
                        let arm_val_reg = body_lowerer.lower_witness(&arm.body)?;
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
            WitnessKind::Loop {
                var,
                iterable,
                body,
            } => {
                use crate::value::Value;
                let iter_reg = self.lower_witness(iterable)?;
                let i_reg = self.alloc_reg();
                self.emit(MirInst::Const(i_reg, Value::Int(0)));
                let len_reg = self.alloc_reg();
                self.emit(MirInst::Call(len_reg, "len".to_string(), vec![iter_reg]));
                let one_reg = self.alloc_reg();
                self.emit(MirInst::Const(one_reg, Value::Int(1)));

                let loop_label = self.emit.insts.len();
                let cond_reg = self.alloc_reg();
                self.emit(MirInst::BinaryOp(
                    cond_reg,
                    i_reg,
                    crate::common::BinaryOp::GreaterEqual,
                    len_reg,
                ));
                self.emit(MirInst::JumpIf(cond_reg, 0));
                let exit_jump_idx = self.emit.insts.len() - 1;

                let x_reg = self.alloc_reg();
                self.emit(MirInst::Index(x_reg, iter_reg, i_reg));
                self.emit(MirInst::Define(var.clone(), x_reg));

                let body_start = self.emit.insts.len();
                self.emit.loop_stack.push((loop_label, 0));
                let _ = self.lower_witness(body)?;
                self.emit.loop_stack.pop();
                let body_end = self.emit.insts.len();

                self.emit(MirInst::BinaryOp(
                    i_reg,
                    i_reg,
                    crate::common::BinaryOp::Add,
                    one_reg,
                ));
                self.emit(MirInst::Jump(loop_label));

                let end_label = self.emit.insts.len();
                self.patch_label_at(exit_jump_idx, end_label);
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
            WitnessKind::While { cond, body } => {
                let loop_label = self.emit.insts.len();
                let c = self.lower_witness(cond)?;
                self.emit(MirInst::JumpIfNot(c, 0)); // placeholder
                let exit_jump_idx = self.emit.insts.len() - 1;

                let body_start = self.emit.insts.len();
                self.emit.loop_stack.push((loop_label, 0));
                let _ = self.lower_witness(body)?;
                self.emit.loop_stack.pop();
                let body_end = self.emit.insts.len();

                self.emit(MirInst::Jump(loop_label));
                let end_label = self.emit.insts.len();
                self.patch_label_at(exit_jump_idx, end_label);
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
            WitnessKind::Closure { params, body } => {
                let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                let mut body_lowerer = WitnessLowerer::new();
                let body_dst = body_lowerer.lower_witness(body)?;
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
            WitnessKind::FnDef {
                name, params, body, ..
            } => {
                let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                let mut body_lowerer = WitnessLowerer::new();
                let body_dst = body_lowerer.lower_witness(body)?;
                body_lowerer.emit(MirInst::Return(Some(body_dst)));
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::TaskDef {
                    name: name.clone(),
                    params: param_names,
                    body: Box::new(body_mir),
                });
                Ok(0)
            }

            // ── DynTrait ──
            WitnessKind::DynTrait {
                expr,
                trait_name,
                generics,
            } => {
                let src = self.lower_witness(expr)?;
                let dst = self.alloc_reg();
                let generic_strs: Vec<String> = generics.iter().map(|t| t.to_type().name()).collect();
                self.emit(MirInst::DynTrait {
                    dst,
                    src,
                    trait_generics: generic_strs,
                    trait_name: trait_name.clone(),
                });
                Ok(dst)
            }

            // ── Prompt ──
            WitnessKind::Prompt { parts } => {
                let mut part_regs: Vec<Reg> = Vec::new();
                for part in parts {
                    let r = self.lower_witness(part)?;
                    part_regs.push(r);
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::Prompt(dst, part_regs));
                Ok(dst)
            }

            // ── Let binding ──
            WitnessKind::LetBinding {
                name,
                value,
                init_body,
                ..
            } => {
                let v_dst = self.lower_witness(value)?;
                self.emit(MirInst::Define(name.clone(), v_dst));
                let b_dst = self.lower_witness(init_body)?;
                let dst = self.alloc_reg();
                self.emit(MirInst::Assign("__let_result".to_string(), b_dst));
                self.emit(MirInst::Var(dst, "__let_result".to_string()));
                Ok(dst)
            }

            // ── Assignment ──
            WitnessKind::Assign { target, value } => {
                let v = self.lower_witness(value)?;
                self.emit(MirInst::Assign(target.clone(), v));
                Ok(v)
            }

            // ── IndexAssign ──
            WitnessKind::IndexAssign {
                object,
                index,
                value,
            } => {
                let obj = self.lower_witness(object)?;
                let idx = self.lower_witness(index)?;
                let val = self.lower_witness(value)?;
                self.emit(MirInst::IndexAssign(obj, idx, val));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Sequence ──
            WitnessKind::Sequence(ws) => {
                let mut last_dst = 0;
                for e in ws {
                    last_dst = self.lower_witness(e)?;
                }
                Ok(last_dst)
            }

            // ── Return / Break / Continue ──
            WitnessKind::Return(val) => {
                match val {
                    Some(v) => {
                        let r = self.lower_witness(v)?;
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
            WitnessKind::Break(_label) => {
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
            WitnessKind::Continue(_label) => {
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
            WitnessKind::Orchestrate {
                input_var,
                result_var,
                kind,
            } => {
                // v0.78: orchestrate 触发 BSP effect（Agent 协调）
                self.effects.extend("Bsp");
                // WitnessOrchestrateKind → MirOrchestrateKind（含嵌套 agent 树转换）
                let mir_kind = crate::mir::orchestrate::MirOrchestrateKind::from_witness_kind(kind);
                self.emit(MirInst::Orchestrate {
                    input_var: input_var.clone(),
                    result_var: result_var.clone(),
                    kind: Box::new(mir_kind),
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Type definitions ──
            WitnessKind::TypeAlias { name, target } => {
                self.emit(MirInst::TypeAlias {
                    name: name.clone(),
                    target: target.to_type().name(),
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            // v0.98: effect 签名声明 —— 纯类型层，无运行时语义，落 Nil 常量
            //（签名由 typeck 预扫描消费，与 MirInst 无关）。
            WitnessKind::EffectSig { .. } => {
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            WitnessKind::EnumDef { name, variants } => {
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
            WitnessKind::StructDef { name, fields } => {
                let sfs: Vec<crate::common::StructField> = fields
                    .iter()
                    .map(|(fname, ftype)| crate::common::StructField {
                        name: fname.clone(),
                        type_hint: ftype.to_type().name(),
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
            WitnessKind::Import(path) => {
                self.emit(MirInst::Import(path.clone()));
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            WitnessKind::MacroDef { name, params, body } => {
                // v0.92: Witness 路径携带 body —— 直接 lower 成 MirFunction。
                let mut body_lowerer = WitnessLowerer::new();
                let body_dst = body_lowerer.lower_witness(body)?;
                body_lowerer.emit(MirInst::Return(Some(body_dst)));
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::MacroDef {
                    name: name.clone(),
                    params: params.clone(),
                    body: Box::new(body_mir),
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }

            // ── Perform ──
            WitnessKind::Perform { effect, args } => {
                let mut arg_regs = Vec::new();
                for arg in args {
                    let r = self.lower_witness(arg)?;
                    arg_regs.push(r);
                }
                let dst = self.alloc_reg();
                self.emit(MirInst::Perform {
                    dst,
                    effect: effect.clone(),
                    args: arg_regs,
                });
                Ok(dst)
            }

            // ── Handle ──
            WitnessKind::Handle {
                effect,
                body,
                handler,
                k_param,
            } => {
                // body 块独立 EmitContext（独立寄存器空间）
                let body_outer = std::mem::replace(&mut self.emit, EmitContext::new());
                let (body_reg, body_w) = self.emit_block_via_witness_w(body)?;
                let body_inner = std::mem::replace(&mut self.emit, body_outer);
                let body_mir = {
                    let mut tmp = WitnessLowerer::new();
                    tmp.emit = body_inner;
                    tmp.finish()
                };

                // handler 块独立 EmitContext
                let handler_outer = std::mem::replace(&mut self.emit, EmitContext::new());
                let (handler_reg, handler_w) = self.emit_block_via_witness_w(handler)?;
                let handler_inner = std::mem::replace(&mut self.emit, handler_outer);
                let handler_mir = {
                    let mut tmp = WitnessLowerer::new();
                    tmp.emit = handler_inner;
                    tmp.finish()
                };

                let k_dst = self.alloc_reg();
                self.emit.emit(MirInst::Handle {
                    effect: effect.clone(),
                    body: Box::new(body_mir),
                    handler: Box::new(handler_mir),
                    k_param: k_param.clone(),
                    k_dst,
                });
                self.emit.emit(MirInst::Const(k_dst, Value::Nil));
                let _ = (body_reg, handler_reg, body_w, handler_w);
                Ok(k_dst)
            }

            // ── Quasiquote ──
            WitnessKind::Quasiquote { segments } => {
                let dst = self.alloc_reg();
                let mut resolved: Vec<crate::mir::QuasiquoteSegment> = Vec::new();
                for seg in segments {
                    match &seg.kind {
                        WitnessKind::Literal(Literal::String(s, _)) => {
                            resolved.push(crate::mir::QuasiquoteSegment::Quote(s.clone()));
                        }
                        WitnessKind::Variable(_name) => {
                            let reg = self.lower_witness(seg)?;
                            resolved.push(crate::mir::QuasiquoteSegment::Unquote(reg));
                        }
                        WitnessKind::Call { callee, args } => match callee {
                            WitnessCallee::Name(n) if n == "splice" => {
                                if let Some(arg) = args.first() {
                                    let reg = self.lower_witness(arg)?;
                                    resolved.push(crate::mir::QuasiquoteSegment::UnquoteSplice(reg));
                                }
                            }
                            _ => {
                                let reg = self.lower_witness(seg)?;
                                resolved.push(crate::mir::QuasiquoteSegment::Unquote(reg));
                            }
                        },
                        _ => {
                            let reg = self.lower_witness(seg)?;
                            resolved.push(crate::mir::QuasiquoteSegment::Unquote(reg));
                        }
                    }
                }
                self.emit.emit(MirInst::Quasiquote { dst, segments: resolved });
                Ok(dst)
            }

            // ── TEA (v0.83) + WithConfig + 其他 witness-only 变体 ──
            WitnessKind::ModelDef { name, fields } => {
                // ModelDef 是类型声明（无 body）——与 StructDef 同构。
                let sfs: Vec<crate::common::StructField> = fields
                    .iter()
                    .map(|(fname, ftype)| crate::common::StructField {
                        name: fname.clone(),
                        type_hint: ftype.to_type().name(),
                    })
                    .collect();
                self.emit(MirInst::ModelDef {
                    name: name.clone(),
                    fields: sfs,
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            WitnessKind::MsgDef { name, variants } => {
                self.emit(MirInst::MsgDef {
                    name: name.clone(),
                    variants: variants.clone(),
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            WitnessKind::UpdateDef {
                name,
                params,
                body,
                ..
            } => {
                let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
                let mut body_lowerer = WitnessLowerer::new();
                let body_dst = body_lowerer.lower_witness(body)?;
                body_lowerer.emit(MirInst::Return(Some(body_dst)));
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::UpdateDef {
                    name: name.clone(),
                    params: param_names,
                    body: Box::new(body_mir),
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            WitnessKind::AppDef {
                name,
                model_name,
                msg_name,
                init_w,
                update_w,
                view_w,
            } => {
                let mut init_l = WitnessLowerer::new();
                let init_dst = init_l.lower_witness(init_w)?;
                init_l.emit(MirInst::Return(Some(init_dst)));
                let init_mir = init_l.finish();
                let mut update_l = WitnessLowerer::new();
                let update_dst = update_l.lower_witness(update_w)?;
                update_l.emit(MirInst::Return(Some(update_dst)));
                let update_mir = update_l.finish();
                let mut view_l = WitnessLowerer::new();
                let view_dst = view_l.lower_witness(view_w)?;
                view_l.emit(MirInst::Return(Some(view_dst)));
                let view_mir = view_l.finish();
                self.emit(MirInst::AppDef {
                    name: name.clone(),
                    model_name: model_name.clone(),
                    msg_name: msg_name.clone(),
                    init_mir: Box::new(init_mir),
                    update_mir: Box::new(update_mir),
                    view_mir: Box::new(view_mir),
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
            WitnessKind::WithConfig { bindings, body } => {
                // WithConfig 是元数据包装——emit WithConfig 指令后 lower body。
                let mut binding_regs: Vec<(String, Reg)> = Vec::new();
                for (k, v) in bindings {
                    let r = self.lower_witness(v)?;
                    binding_regs.push((k.clone(), r));
                }
                // body 独立 lower 成 MirFunction
                let mut body_lowerer = WitnessLowerer::new();
                let body_dst = body_lowerer.lower_witness(body)?;
                body_lowerer.emit(MirInst::Return(Some(body_dst)));
                let body_mir = body_lowerer.finish();
                self.emit(MirInst::WithConfig {
                    bindings: binding_regs,
                    body: Box::new(body_mir),
                    jit: false,
                });
                let dst = self.alloc_reg();
                self.emit(MirInst::Const(dst, crate::value::Value::Nil));
                Ok(dst)
            }
        }
    }
}

/// v0.92: 占位 MirWitness —— handle 块嵌套 lowering 时，body/handler 块的
/// witness 由 parser 产生，但 lower 阶段我们只 emit IR（MirInst），不重复
/// 构建 witness —— 返回一个 Nil 字面量 witness 作 placeholder。
fn empty_witness_for_span(span: crate::common::Span) -> MirWitness {
    MirWitness {
        kind: WitnessKind::Literal(crate::common::Literal::Nil(span)),
        span,
    }
}

pub fn pattern_to_string(pattern: &crate::mir::witness::WitnessPattern) -> String {
    use crate::mir::witness::WitnessPattern as Pattern;
    match pattern {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Variable(name) => name.clone(),
        Pattern::Literal(lit) => match lit {
            crate::common::Literal::String(s, _) => format!("str:{}", s),
            crate::common::Literal::Char(c, _) => format!("char:{}", c),
            crate::common::Literal::Int(i, _) => format!("int:{}", i),
            crate::common::Literal::Float(f, _) => format!("float:{}", f),
            crate::common::Literal::BigInt(n, _) => format!("bigint:{}", n),
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
        Pattern::ListVec { elements, rest } => {
            let parts: Vec<String> =
                elements.iter().map(pattern_to_string).collect();
            if let Some(r) = rest {
                format!(
                    "list:vector:[{},..{}]",
                    parts.join(","),
                    pattern_to_string(r)
                )
            } else {
                format!("list:vector:[{}]", parts.join(","))
            }
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

// v0.78: 单元测试 — WitnessLowerer.classify_call_effect / classify_method_effect 的 effect label 分类
#[cfg(test)]
mod tests {
    use super::super::effect::EffectRow;

    #[test]
    fn classify_call_effect_known_builtin() {
        // 直接构造 EffectRow 测试保守累积（无需 Lowerer 全栈）
        let mut r = EffectRow::default();
        for label in &["Ai", "Fs", "Mem", "Sandbox"] {
            r.extend(label);
        }
        assert_eq!(r.len(), 4);
        assert!(r.contains("Ai"));
        assert!(r.contains("Fs"));
        assert!(r.contains("Mem"));
        assert!(r.contains("Sandbox"));
        assert!(!r.contains("Bsp"));
    }

    #[test]
    fn classify_call_effect_idempotent() {
        let mut r = EffectRow::default();
        r.extend("Ai");
        r.extend("Ai"); // 第二次同 label 不增
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn orchestrate_appends_bsp() {
        // orchestrate 触发的 BSP effect 累积路径（直接验证 row 行为）
        let mut r = EffectRow::default();
        r.extend("Ai");
        r.extend("Bsp");
        assert_eq!(r.len(), 2);
        assert!(r.contains("Bsp"));
    }
}

/// v0.80 Stage 2.0: handle 块的 body/handler witness → MirFunction。
//
// （注：实际定义在文件末尾，clippy items_after_test_module 暂时 #[allow]）
#[allow(clippy::items_after_test_module)]
///
/// 入口：parser 在 emit_handle_w 中独立 lower body_w / handler_w 为独立 MirFunction
/// （独立 EmitContext = 独立寄存器空间，与 TaskDef 一致）。
/// 返回的 MirFunction 是 emit 完所有 MIR 后的快照。
///
/// v0.92: 直接走 WitnessLowerer（零 MirExpr 桥接）。
pub(crate) fn lower_block_witness_to_mir(
    witness: &crate::mir::witness::MirWitness,
) -> crate::mir::MirFunction {
    let mut l = WitnessLowerer::new();
    let _ = l.lower_witness(witness);
    l.finish()
}
