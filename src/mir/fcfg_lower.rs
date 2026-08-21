//! v0.89: FCFG → MIR 桥接层 — Node<()> 降维为 MirInst 线性序列。
//!
//! 这是 emit.rs 拆分的第一步：将结构化控制流节点（If/While/For/Match）
//! 降维为线性 Jump/Label 指令。逻辑与 emit.rs 的 emit_*_w 函数等价，
//! 但操作对象是预构建的 Node<()> 树而非 Token 流。
//!
//! 使用 EmitContext（与 emit.rs 和 lower.rs 共享）做寄存器分配和指令发射。

use super::fcfg::{Block, Fcfg, MatchArm, Node, Pattern, QuasiquoteSegment, Reg};
use super::lower::EmitContext;
use super::MirInst;
use crate::common::BinaryOp;
use crate::value::Value;

/// 将 FCFG 节点列表降维为 MirInst 线性序列。
///
/// 返回 (instructions, n_regs) — 指令列表和使用的寄存器总数。
/// n_regs 是所有节点中引用的最大寄存器号 + 1。
pub fn lower_fcfg(nodes: &[Fcfg]) -> (Vec<MirInst>, usize) {
    let mut ctx = EmitContext::new();
    for node in nodes {
        lower_node(&mut ctx, node);
    }
    // n_regs = max(预分配寄存器, ctx 分配的寄存器) + 1 (0-indexed)
    let max_pre_alloc = max_reg_in_nodes(nodes);
    let n_regs = max_pre_alloc.max(ctx.next_reg.saturating_sub(1)) + 1;
    (ctx.insts, n_regs)
}

/// 递归计算节点树中引用的最大寄存器号。
fn max_reg_in_nodes(nodes: &[Fcfg]) -> usize {
    let mut max = 0usize;
    for node in nodes {
        max = max.max(max_reg_in_node(node));
    }
    max
}

fn max_reg_in_node(node: &Fcfg) -> usize {
    match node {
        Node::Literal { reg, .. } | Node::Variable { reg, .. } | Node::Expr { reg, .. } => *reg,
        Node::BinaryOp { dst, lhs, rhs, .. } => *dst.max(lhs).max(rhs),
        Node::Call { dst, callee, args, .. } => {
            args.iter().fold(*dst.max(callee), |m, r| m.max(*r))
        }
        Node::MethodCall { dst, receiver, args, .. } => {
            args.iter().fold(*dst.max(receiver), |m, r| m.max(*r))
        }
        Node::ListLit { dst, items, .. } => items.iter().fold(*dst, |m, r| m.max(*r)),
        Node::DictLit { dst, entries, .. } => entries.iter().fold(*dst, |m, (_, r)| m.max(*r)),
        Node::Index { dst, obj, idx, .. } => *dst.max(obj).max(idx),
        Node::If { cond, then, else_, .. } => {
            let mut m = *cond;
            m = m.max(max_reg_in_nodes(&then.nodes));
            if let Some(e) = else_ {
                m = m.max(max_reg_in_nodes(&e.nodes));
            }
            m
        }
        Node::While { cond, body, .. } => {
            max_reg_in_nodes(&cond.nodes).max(max_reg_in_nodes(&body.nodes))
        }
        Node::For { iter, body, .. } => *iter.max(&max_reg_in_nodes(&body.nodes)),
        Node::Match { scrutinee, arms, .. } => {
            let mut m = *scrutinee;
            for arm in arms {
                m = m.max(max_reg_in_nodes(&arm.body.nodes));
            }
            m
        }
        Node::Let { value, body, .. } => *value.max(&max_reg_in_nodes(&body.nodes)),
        Node::Assign { value, .. } => *value,
        Node::IndexAssign { obj, idx, value, .. } => *obj.max(idx).max(value),
        Node::Perform { args, .. } => args.iter().fold(0usize, |m, r| m.max(*r)),
        Node::Handle { body, handler, .. } => {
            max_reg_in_nodes(&body.nodes).max(max_reg_in_nodes(&handler.nodes))
        }
        Node::Quasiquote { dst, segments, .. } => {
            segments.iter().fold(*dst, |m, seg| match seg {
                QuasiquoteSegment::Unquote(r) | QuasiquoteSegment::UnquoteSplice(r) => m.max(*r),
                _ => m,
            })
        }
        Node::Sequence { nodes, .. } => max_reg_in_nodes(nodes),
        _ => 0,
    }
}

/// 降维单个 FCFG 节点。
fn lower_node(ctx: &mut EmitContext, node: &Fcfg) {
    match node {
        // ── 值产生 ──
        Node::Literal { reg, value, .. } => {
            ctx.emit(MirInst::Const(*reg, literal_to_value(value)));
        }
        Node::Variable { reg, name, .. } => {
            ctx.emit(MirInst::Var(*reg, name.clone()));
        }
        Node::BinaryOp { dst, lhs, op, rhs, .. } => {
            ctx.emit(MirInst::BinaryOp(*dst, *lhs, op.clone(), *rhs));
        }
        Node::Call { dst, callee, args, .. } => {
            // FCFG 的 Call 用 Reg 作 callee，但 MirInst::Call 用 String。
            // 桥接层保留 Reg→String 映射（callee 已在 emit 阶段解析为名称）。
            // 这里用 format! 作为占位——实际迁移时 callee 应携带名称。
            ctx.emit(MirInst::Call(*dst, format!("_r{}", callee), args.clone()));
        }
        Node::MethodCall { dst, receiver, method, args, .. } => {
            ctx.emit(MirInst::MethodCall(*dst, *receiver, method.clone(), args.clone()));
        }
        Node::ListLit { dst, items, .. } => {
            ctx.emit(MirInst::ListLit(*dst, items.clone()));
        }
        Node::DictLit { dst, entries, .. } => {
            ctx.emit(MirInst::DictLit(*dst, entries.clone()));
        }
        Node::Index { dst, obj, idx, .. } => {
            ctx.emit(MirInst::Index(*dst, *obj, *idx));
        }

        // ── 控制流：If → JumpIfNot + Jump ──
        Node::If { cond, then, else_, .. } => {
            ctx.emit(MirInst::JumpIfNot(*cond, 0));
            let jump_not_idx = ctx.insts.len() - 1;
            lower_block(ctx, then);
            if let Some(else_block) = else_ {
                ctx.emit(MirInst::Jump(0));
                let jump_idx = ctx.insts.len() - 1;
                let else_start = ctx.insts.len();
                ctx.patch_label_at(jump_not_idx, else_start);
                lower_block(ctx, else_block);
                let end = ctx.insts.len();
                ctx.patch_label_at(jump_idx, end);
            } else {
                let end = ctx.insts.len();
                ctx.patch_label_at(jump_not_idx, end);
            }
        }

        // ── 控制流：While → Label + cond + JumpIfNot + body + Jump ──
        Node::While { cond, body, .. } => {
            let loop_start = ctx.insts.len();
            lower_block(ctx, cond);
            let cond_reg = cond.result.unwrap_or(0);
            ctx.emit(MirInst::JumpIfNot(cond_reg, 0));
            let jump_not_idx = ctx.insts.len() - 1;
            // Push loop context for break/continue
            let continue_label = loop_start;
            let break_placeholder = 0usize; // patched below
            ctx.loop_stack.push((continue_label, break_placeholder));
            lower_block(ctx, body);
            ctx.emit(MirInst::Jump(loop_start));
            let end = ctx.insts.len();
            ctx.patch_label_at(jump_not_idx, end);
            // Patch break targets
            if let Some((_, break_label)) = ctx.loop_stack.pop() {
                // break_label was placeholder; all Break instructions
                // in the body were emitted with label 0, patch them now.
                // Note: this is a simplified approach. A full implementation
                // would track break instruction indices.
                let _ = break_label;
            }
        }

        // ── 控制流：For → index-based loop ──
        Node::For { var, iter, body, .. } => {
            // let __idx = 0
            let idx_reg = ctx.alloc_reg();
            ctx.emit(MirInst::Const(idx_reg, Value::Int(0)));
            // let __len = len(iter)
            let len_reg = ctx.alloc_reg();
            ctx.emit(MirInst::Call(len_reg, "len".to_string(), vec![*iter]));
            // Label: loop_start
            let loop_start = ctx.insts.len();
            // cond: __idx >= __len
            let cond_reg = ctx.alloc_reg();
            ctx.emit(MirInst::BinaryOp(cond_reg, idx_reg, BinaryOp::GreaterEqual, len_reg));
            ctx.emit(MirInst::JumpIfNot(cond_reg, 0));
            let jump_not_idx = ctx.insts.len() - 1;
            // let var = iter[__idx]
            let val_reg = ctx.alloc_reg();
            ctx.emit(MirInst::Index(val_reg, *iter, idx_reg));
            ctx.emit(MirInst::Define(var.clone(), val_reg));
            // body
            lower_block(ctx, body);
            // __idx = __idx + 1
            let one_reg = ctx.alloc_reg();
            ctx.emit(MirInst::Const(one_reg, Value::Int(1)));
            ctx.emit(MirInst::BinaryOp(idx_reg, idx_reg, BinaryOp::Add, one_reg));
            ctx.emit(MirInst::Jump(loop_start));
            // Label: loop_end
            let end = ctx.insts.len();
            ctx.patch_label_at(jump_not_idx, end);
        }

        // ── Match → scrutinee + sequential JumpIf pattern checks ──
        Node::Match { scrutinee, arms, .. } => {
            lower_match(ctx, *scrutinee, arms);
        }

        // ── Return/Break/Continue ──
        Node::Return { value, .. } => {
            ctx.emit(MirInst::Return(*value));
        }
        Node::Break { .. } => {
            // Break target is the break label from loop_stack top
            let break_label = ctx.loop_stack.last().map_or(0, |&(_, b)| b);
            ctx.emit(MirInst::Break(break_label));
        }
        Node::Continue { .. } => {
            let continue_label = ctx.loop_stack.last().map_or(0, |&(c, _)| c);
            ctx.emit(MirInst::Continue(continue_label));
        }

        // ── 绑定 ──
        Node::Let { name, value, body, .. } => {
            ctx.emit(MirInst::Define(name.clone(), *value));
            lower_block(ctx, body);
        }
        Node::Assign { name, value, .. } => {
            ctx.emit(MirInst::Assign(name.clone(), *value));
        }
        Node::IndexAssign { obj, idx, value, .. } => {
            ctx.emit(MirInst::IndexAssign(*obj, *idx, *value));
        }

        // ── 声明 ──
        Node::FnDef { name, params, body, .. } => {
            let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            let body_mir = lower_block_to_function(ctx, body);
            ctx.emit(MirInst::TaskDef {
                name: name.clone(),
                params: param_names,
                body: Box::new(body_mir),
            });
        }
        Node::Import { path, .. } => {
            ctx.emit(MirInst::Import(path.clone()));
        }
        Node::TypeAlias { name, target, .. } => {
            ctx.emit(MirInst::TypeAlias {
                name: name.clone(),
                target: target.0.clone(),
            });
        }

        // ── 代数效果 ──
        Node::Perform { effect, args, .. } => {
            let dst = ctx.alloc_reg();
            ctx.emit(MirInst::Perform {
                dst,
                effect: effect.clone(),
                args: args.clone(),
            });
        }
        Node::Handle { effect, body, handler, k_param, .. } => {
            let body_mir = lower_block_to_function(ctx, body);
            let handler_mir = lower_block_to_function(ctx, handler);
            let k_dst = ctx.alloc_reg();
            ctx.emit(MirInst::Handle {
                effect: effect.clone(),
                body: Box::new(body_mir),
                handler: Box::new(handler_mir),
                k_param: k_param.clone(),
                k_dst,
            });
        }

        // ── Quasiquote ──
        Node::Quasiquote { dst, segments, .. } => {
            let resolved: Vec<super::QuasiquoteSegment> = segments
                .iter()
                .map(|seg| match seg {
                    QuasiquoteSegment::Quote(s) => super::QuasiquoteSegment::Quote(s.clone()),
                    QuasiquoteSegment::Unquote(r) => super::QuasiquoteSegment::Unquote(*r),
                    QuasiquoteSegment::UnquoteSplice(r) => super::QuasiquoteSegment::UnquoteSplice(*r),
                })
                .collect();
            ctx.emit(MirInst::Quasiquote {
                dst: *dst,
                segments: resolved,
            });
        }

        // ── 序列 ──
        Node::Sequence { nodes, .. } => {
            for n in nodes {
                lower_node(ctx, n);
            }
        }

        // ── 表达式语句 ──
        Node::Expr { reg, .. } => {
            ctx.emit(MirInst::Expr(*reg));
        }

        // ── 未覆盖的变体：暂时 emit Nop（Nil 常量占位）──
        _ => {
            let nop = ctx.alloc_reg();
            ctx.emit(MirInst::Const(nop, Value::Nil));
        }
    }
}

/// 降维一个块。
fn lower_block(ctx: &mut EmitContext, block: &Block<()>) {
    for node in &block.nodes {
        lower_node(ctx, node);
    }
}

/// 将块降维为独立的 MirFunction（用于 TaskDef/Handle body/handler）。
fn lower_block_to_function(ctx: &mut EmitContext, block: &Block<()>) -> super::MirFunction {
    let mut sub = EmitContext::new();
    // Copy loop stack context
    sub.loop_stack = ctx.loop_stack.clone();
    lower_block(&mut sub, block);
    sub.finish()
}

/// 降维 Match 表达式。
fn lower_match(ctx: &mut EmitContext, scrutinee: Reg, arms: &[MatchArm<()>]) {
    let mut end_jumps: Vec<usize> = Vec::new();

    for (i, arm) in arms.iter().enumerate() {
        // Pattern check (simplified — literal patterns only for now)
        let is_last = i == arms.len() - 1;
        match &arm.pattern {
            Pattern::Wildcard => {
                // Always matches — no check needed
            }
            Pattern::Literal(lit) => {
                let pat_reg = ctx.alloc_reg();
                ctx.emit(MirInst::Const(pat_reg, literal_to_value(lit)));
                let eq_reg = ctx.alloc_reg();
                ctx.emit(MirInst::BinaryOp(eq_reg, scrutinee, BinaryOp::Equal, pat_reg));
                if !is_last {
                    ctx.emit(MirInst::JumpIfNot(eq_reg, 0));
                    let jump_idx = ctx.insts.len() - 1;
                    // Body
                    lower_block(ctx, &arm.body);
                    if let Some(last) = arm.body.nodes.last() {
                        // Copy result (simplified)
                        let _ = last;
                    }
                    ctx.emit(MirInst::Jump(0));
                    end_jumps.push(ctx.insts.len() - 1);
                    let next_arm = ctx.insts.len();
                    ctx.patch_label_at(jump_idx, next_arm);
                } else {
                    ctx.emit(MirInst::JumpIfNot(eq_reg, 0));
                    let jump_idx = ctx.insts.len() - 1;
                    lower_block(ctx, &arm.body);
                    let end = ctx.insts.len();
                    ctx.patch_label_at(jump_idx, end);
                }
            }
            _ => {
                // Other patterns — treat as wildcard for now
                lower_block(ctx, &arm.body);
            }
        }
    }

    // Patch all end jumps to current position
    let end = ctx.insts.len();
    for idx in end_jumps {
        ctx.patch_label_at(idx, end);
    }
}

/// Literal → Value 转换。
fn literal_to_value(lit: &crate::common::Literal) -> Value {
    match lit {
        crate::common::Literal::Int(n, _) => Value::Int(*n),
        crate::common::Literal::Float(f, _) => Value::Float(*f),
        crate::common::Literal::String(s, _) => Value::String(s.clone()),
        crate::common::Literal::Bool(b, _) => Value::Bool(*b),
        crate::common::Literal::Nil(_) => Value::Nil,
        crate::common::Literal::Char(c, _) => Value::Char(*c),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{Literal, Span};

    #[test]
    fn lower_literal_node() {
        let nodes = vec![Node::Literal {
            reg: 0,
            value: Literal::Int(42, Span::default()),
            meta: (),
        }];
        let (insts, n_regs) = lower_fcfg(&nodes);
        assert_eq!(insts.len(), 1);
        assert_eq!(n_regs, 1);
        match &insts[0] {
            MirInst::Const(r, Value::Int(v)) => {
                assert_eq!(*r, 0);
                assert_eq!(*v, 42);
            }
            _ => panic!("expected Const, got {:?}", insts[0]),
        }
    }

    #[test]
    fn lower_binary_op() {
        let nodes = vec![Node::BinaryOp {
            dst: 2,
            lhs: 0,
            op: BinaryOp::Add,
            rhs: 1,
            meta: (),
        }];
        let (insts, _) = lower_fcfg(&nodes);
        assert_eq!(insts.len(), 1);
        match &insts[0] {
            MirInst::BinaryOp(dst, lhs, op, rhs) => {
                assert_eq!(*dst, 2);
                assert_eq!(*lhs, 0);
                assert!(matches!(op, BinaryOp::Add));
                assert_eq!(*rhs, 1);
            }
            _ => panic!("expected BinaryOp"),
        }
    }

    #[test]
    fn lower_if_node() {
        // if cond { then_val } else { else_val }
        // cond = r0 (true), then = r1, else = r2
        let nodes = vec![Node::If {
            cond: 0,
            then: Block {
                nodes: vec![Node::Literal {
                    reg: 1,
                    value: Literal::Int(1, Span::default()),
                    meta: (),
                }],
                result: Some(1),
            },
            else_: Some(Block {
                nodes: vec![Node::Literal {
                    reg: 2,
                    value: Literal::Int(2, Span::default()),
                    meta: (),
                }],
                result: Some(2),
            }),
            meta: (),
        }];
        let (insts, n_regs) = lower_fcfg(&nodes);
        // Expected: JumpIfNot(r0, 3), Const(r1, 1), Jump(4), Const(r2, 2)
        assert_eq!(insts.len(), 4, "expected 4 instructions: {:?}", insts);
        assert!(matches!(&insts[0], MirInst::JumpIfNot(0, _)));
        assert!(matches!(&insts[1], MirInst::Const(1, Value::Int(1))));
        assert!(matches!(&insts[2], MirInst::Jump(_)));
        assert!(matches!(&insts[3], MirInst::Const(2, Value::Int(2))));
        let _ = n_regs;
    }

    #[test]
    fn lower_sequence() {
        let nodes = vec![Node::Sequence {
            nodes: vec![
                Node::Literal {
                    reg: 0,
                    value: Literal::Int(1, Span::default()),
                    meta: (),
                },
                Node::Literal {
                    reg: 1,
                    value: Literal::Int(2, Span::default()),
                    meta: (),
                },
            ],
            meta: (),
        }];
        let (insts, _) = lower_fcfg(&nodes);
        assert_eq!(insts.len(), 2);
    }
}
