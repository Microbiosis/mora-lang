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
///
/// 寄存器安全：节点携带 witness_to_fcfg 预分配的寄存器号（0..=max），
/// 本层的 bump 分配器必须从 max+1 起步 —— 否则哨兵/临时寄存器
///（__let_result dst、MatchExpr dst、for-loop 索引等）会覆盖
/// 预分配寄存器中的值。
pub fn lower_fcfg(nodes: &[Fcfg]) -> (Vec<MirInst>, usize) {
    let mut ctx = EmitContext::new();
    ctx.next_reg = max_reg_in_nodes(nodes) + 1; // 避开预分配寄存器
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
        Node::Or { dst, lhs, rhs, .. } | Node::And { dst, lhs, rhs, .. } => {
            *dst.max(lhs).max(rhs)
        }
        Node::DynTrait { dst, src, .. } => *dst.max(src),
        Node::Prompt { dst, parts, .. } => parts.iter().fold(*dst, |m, r| m.max(*r)),
        Node::ClosureExpr { dst, body, .. } => *dst.max(&max_reg_in_nodes(&body.nodes)),
        Node::ListLit { dst, items, .. } => items.iter().fold(*dst, |m, r| m.max(*r)),
        Node::DictLit { dst, entries, .. } => entries.iter().fold(*dst, |m, (_, r)| m.max(*r)),
        Node::Index { dst, obj, idx, .. } => *dst.max(obj).max(idx),
        // dst 是 witness_to_fcfg 预分配的结果寄存器，必须计入 max ——
        // 否则本层 bump 分配器会覆盖它（寄存器安全契约见 lower_fcfg 文档）。
        Node::If { cond, then, else_, dst, .. } => {
            let mut m = (*cond).max(*dst);
            m = m.max(max_reg_in_nodes(&then.nodes));
            if let Some(e) = else_ {
                m = m.max(max_reg_in_nodes(&e.nodes));
            }
            m
        }
        // dst 是 witness_to_fcfg 预分配的循环结果寄存器，必须计入 max ——
        // 否则本层 bump 分配器会覆盖它（寄存器安全契约见 lower_fcfg 文档）。
        Node::While { cond, body, dst, .. } => *dst
            .max(&max_reg_in_nodes(&cond.nodes))
            .max(&max_reg_in_nodes(&body.nodes)),
        Node::For { iter, body, dst, .. } => {
            (*iter).max(*dst).max(max_reg_in_nodes(&body.nodes))
        }
        Node::Match { dst, scrutinee, arms, .. } => {
            let mut m = *dst.max(scrutinee);
            for arm in arms {
                m = m.max(max_reg_in_nodes(&arm.body.nodes));
            }
            m
        }
        Node::Let { value, body, .. } => *value.max(&max_reg_in_nodes(&body.nodes)),
        Node::Assign { value, .. } => *value,
        Node::IndexAssign { obj, idx, value, .. } => *obj.max(idx).max(value),
        Node::Perform { dst, args, .. } => args.iter().fold(*dst, |m, r| m.max(*r)),
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

/// 发射「循环作为表达式」的结果常量。
///
/// emit.rs 的 `emit_loop_w` / `emit_while_w` 在循环指令流末尾各留一个
/// `Const(dst, Nil)`，使 for/while 出现在值位置（如 `let x = for ... end`）
/// 时消费者能读到确定的值。9 层管线此前不发这一条 —— 结果是：
/// ① 类别差分因少了 `Const` 而失败 → 管线对含循环的模块永久回落
/// emit.rs；② `node_result_reg` 读不到循环结果寄存器。
fn emit_loop_result(ctx: &mut EmitContext, dst: Reg) {
    ctx.emit(MirInst::Const(dst, Value::Nil));
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
        Node::Call { dst, callee, callee_name, args, .. } => {
            let name = callee_name
                .clone()
                .unwrap_or_else(|| format!("_r{}", callee));
            ctx.emit(MirInst::Call(*dst, name, args.clone()));
        }
        Node::MethodCall { dst, receiver, method, args, .. } => {
            ctx.emit(MirInst::MethodCall(*dst, *receiver, method.clone(), args.clone()));
        }
        // Or/And → 短路求值（镜像 emit_or_w 的 JumpIf + NotEqual 模式）
        Node::Or { dst, lhs, rhs, .. } => {
            let l = *lhs;
            ctx.emit(MirInst::JumpIf(l, 0));
            let jump_idx = ctx.insts.len() - 1;
            ctx.emit(MirInst::BinaryOp(*dst, l, BinaryOp::NotEqual, *rhs));
            let end = ctx.insts.len();
            ctx.patch_label_at(jump_idx, end);
        }
        Node::And { dst, lhs, rhs, .. } => {
            let l = *lhs;
            ctx.emit(MirInst::JumpIfNot(l, 0));
            let jump_idx = ctx.insts.len() - 1;
            ctx.emit(MirInst::BinaryOp(*dst, l, BinaryOp::Equal, *rhs));
            let end = ctx.insts.len();
            ctx.patch_label_at(jump_idx, end);
        }
        Node::DynTrait { dst, src, trait_name, .. } => {
            ctx.emit(MirInst::DynTrait {
                dst: *dst,
                src: *src,
                trait_generics: vec![],
                trait_name: trait_name.clone(),
            });
        }
        Node::Prompt { dst, parts, .. } => {
            ctx.emit(MirInst::Prompt(*dst, parts.clone()));
        }
        Node::ClosureExpr { dst, params, body, .. } => {
            let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            let body_mir = lower_body_function_with_return(ctx, body);
            ctx.emit(MirInst::Closure {
                dst: *dst,
                params: param_names,
                body: Box::new(body_mir),
            });
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

        // ── 控制流：If → JumpIfNot + Copy 结果合并（镜像 emit 路径）──
        Node::If { cond, then, else_, dst, .. } => {
            ctx.emit(MirInst::JumpIfNot(*cond, 0));
            let jump_not_idx = ctx.insts.len() - 1;
            lower_block(ctx, then);
            let then_result = then.result.unwrap_or(0);
            // v0.104.2: 用节点自带的结果寄存器（witness_to_fcfg 预分配），
            // 不再就地 alloc —— 消费者（`let x = if … end`）读的是同一个
            // dst；就地分配时它无法得知该寄存器号，只能回退哨兵 0。
            ctx.emit(MirInst::Copy(*dst, then_result));
            if let Some(else_block) = else_ {
                ctx.emit(MirInst::Jump(0));
                let jump_idx = ctx.insts.len() - 1;
                let else_start = ctx.insts.len();
                ctx.patch_label_at(jump_not_idx, else_start);
                lower_block(ctx, else_block);
                let else_result = else_block.result.unwrap_or(0);
                ctx.emit(MirInst::Copy(*dst, else_result));
                let end = ctx.insts.len();
                ctx.patch_label_at(jump_idx, end);
            } else {
                let end = ctx.insts.len();
                ctx.patch_label_at(jump_not_idx, end);
            }
        }

        // ── 控制流：While → Label + cond + JumpIfNot + body + Jump + post-patch ──
        Node::While { cond, body, dst, .. } => {
            let loop_start = ctx.insts.len();
            lower_block(ctx, cond);
            let cond_reg = cond.result.unwrap_or(0);
            ctx.emit(MirInst::JumpIfNot(cond_reg, 0));
            let jump_not_idx = ctx.insts.len() - 1;
            // v0.90.4: break/continue label 由 witness_to_fcfg 携带（Node::Break.label），
            // fcfg_lower 不维护独立 loop_stack —— 后修补直接用节点自带的 label。
            let body_start = ctx.insts.len();
            lower_block(ctx, body);
            let body_end = ctx.insts.len();
            ctx.emit(MirInst::Jump(loop_start));
            let end = ctx.insts.len();
            ctx.patch_label_at(jump_not_idx, end);
            // 后修补：body 内 Break→end_label（label已正确），Continue→loop_start
            for i in body_start..body_end {
                match &mut ctx.insts[i] {
                    MirInst::Break(lbl) => *lbl = end,
                    MirInst::Continue(lbl) => *lbl = loop_start,
                    _ => {}
                }
            }
            // 循环作为表达式的结果 = Nil（与 emit.rs emit_while_w 同契约）。
            emit_loop_result(ctx, *dst);
        }

        // ── 控制流：For → index-based loop + post-patch ──
        Node::For { var, iter, body, dst, .. } => {
            // let __idx = 0
            let idx_reg = ctx.alloc_reg();
            ctx.emit(MirInst::Const(idx_reg, Value::Int(0)));
            // let __len = len(iter)
            let len_reg = ctx.alloc_reg();
            ctx.emit(MirInst::Call(len_reg, "len".to_string(), vec![*iter]));
            // let __one = 1（与 emit.rs 同序：步长常量在循环标签前发射。
            // 位置若后移，指令类别序列与 emit.rs 分歧 → 9 层差分失败 →
            // 管线对 for 循环永久回落，Phase 2 执行器切换被静默阻塞）。
            let one_reg = ctx.alloc_reg();
            ctx.emit(MirInst::Const(one_reg, Value::Int(1)));
            // Label: loop_start
            let loop_start = ctx.insts.len();
            // cond: __idx >= __len —— 这是**退出**条件（不是继续条件）。
            // v0.103 修复：此前此处误用 `JumpIfNot`（与 While 的「cond 为继续
            // 条件」同形写法），而 For 的 cond 是退出条件 —— 语义反转导致
            // `idx < len` 时直接跳出，循环体一次都不执行（`for x in [1,2,3]`
            // 在 9 层管线下降级为 0 次迭代；emit.rs 路径用 `JumpIf` 是正确的，
            // 这是两条管线的语义分歧）。
            // 正确：`cond` 为真（idx >= len）→ 跳到 end；否则落入循环体。
            let cond_reg = ctx.alloc_reg();
            ctx.emit(MirInst::BinaryOp(cond_reg, idx_reg, BinaryOp::GreaterEqual, len_reg));
            ctx.emit(MirInst::JumpIf(cond_reg, 0));
            let exit_jump_idx = ctx.insts.len() - 1;
            // let var = iter[__idx]
            let val_reg = ctx.alloc_reg();
            ctx.emit(MirInst::Index(val_reg, *iter, idx_reg));
            ctx.emit(MirInst::Define(var.clone(), val_reg));
            // v0.90.4: break/continue label 由 Node 携带，无独立 loop_stack
            let body_start = ctx.insts.len();
            lower_block(ctx, body);
            let body_end = ctx.insts.len();
            // __idx = __idx + 1
            // v0.104.2: `continue` 的目标是**这条增量**（见下方后修补）——
            // 跳到 loop_start（条件判定）会跳过增量 → 索引永不前进 → 死循环。
            let increment_idx = ctx.insts.len();
            ctx.emit(MirInst::BinaryOp(idx_reg, idx_reg, BinaryOp::Add, one_reg));
            ctx.emit(MirInst::Jump(loop_start));
            let end = ctx.insts.len();
            ctx.patch_label_at(exit_jump_idx, end);
            // 循环作为表达式的结果 = Nil，写入节点自带的 dst（emit.rs
            // emit_loop_w 的同一契约）。dst 由 witness_to_fcfg 预分配并被
            // 外层 Sequence 的 node_result_reg 读到 —— 写在值位置的 for/while
            //（`let x = for ... end`）消费者才能读到正确寄存器，而不是回退 0。
            emit_loop_result(ctx, *dst);
            // 后修补：body 内 Break→end_label，Continue→增量
            for i in body_start..body_end {
                match &mut ctx.insts[i] {
                    MirInst::Break(lbl) => *lbl = end,
                    MirInst::Continue(lbl) => *lbl = increment_idx,
                    _ => {}
                }
            }
        }

        // ── Match → 单条 MatchExpr（镜像 emit_match_w：嵌套 arm MirFunction）──
        Node::Match { dst, scrutinee, arms, .. } => {
            lower_match(ctx, *scrutinee, *dst, arms);
        }

        // ── Return/Break/Continue ──
        Node::Return { value, .. } => {
            ctx.emit(MirInst::Return(*value));
        }
        Node::Break { label, .. } => {
            // v0.90.4: label 由 witness_to_fcfg 携带（While/For push_loop 后填）。
            // fcfg_lower 无独立 loop_stack——单一信息源：节点本身。
            ctx.emit(MirInst::Break(*label));
        }
        Node::Continue { label, .. } => {
            ctx.emit(MirInst::Continue(*label));
        }

        // ── 绑定 ──
        Node::Let { name, value, body, .. } => {
            ctx.emit(MirInst::Define(name.clone(), *value));
            // 镜像 emit_let_w：init_body（Nil 字面量块）先求值，
            // 结果经 __let_result 哨兵传出 —— let 表达式的值语义。
            lower_block(ctx, body);
            let body_result = body.result.unwrap_or(0);
            ctx.emit(MirInst::Assign("__let_result".to_string(), body_result));
            let dst = ctx.alloc_reg();
            ctx.emit(MirInst::Var(dst, "__let_result".to_string()));
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
            let body_mir = lower_body_function_with_return(ctx, body);
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
        Node::Perform { dst, effect, args, .. } => {
            ctx.emit(MirInst::Perform {
                dst: *dst,
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

        // ── 声明类（编译期注册，运行时无操作）──
        Node::EnumDef { name, variants, .. } => {
            let vars: Vec<crate::common::EnumVariant> = variants
                .iter()
                .map(|v| crate::common::EnumVariant {
                    name: v.name.clone(),
                    data: v.payload.as_ref().map(|p| p.0.clone()),
                })
                .collect();
            ctx.emit(MirInst::EnumDef {
                name: name.clone(),
                variants: vars,
            });
        }
        Node::StructDef { name, fields, .. } => {
            let flds: Vec<crate::common::StructField> = fields
                .iter()
                .map(|(n, t)| crate::common::StructField {
                    name: n.clone(),
                    type_hint: t.0.clone(),
                })
                .collect();
            ctx.emit(MirInst::StructDef {
                name: name.clone(),
                fields: flds,
            });
        }
        Node::TraitDef { name, methods, .. } => {
            let meths: Vec<super::orchestrate::MirTraitMethod> = methods
                .iter()
                .map(|m| super::orchestrate::MirTraitMethod {
                    name: m.name.clone(),
                    params: vec![], // FCFG Param → MirTraitMethod Param 需要额外转换
                    return_type: m.return_ann.as_ref().map(|r| r.0.clone()),
                    body: None,
                })
                .collect();
            ctx.emit(MirInst::TraitDef {
                name: name.clone(),
                parents: vec![],
                methods: meths,
                method_bodies: vec![],
            });
        }
        Node::ImplDef { trait_name, for_type, methods, .. } => {
            let fndefs: Vec<super::orchestrate::MirFnDef> = methods
                .iter()
                .map(|(n, b)| super::orchestrate::MirFnDef {
                    name: n.clone(),
                    params: vec![],
                    return_type: None,
                    body: Some(lower_block_to_function(ctx, b)),
                })
                .collect();
            ctx.emit(MirInst::ImplDef {
                trait_name: trait_name.clone(),
                trait_generics: vec![],
                for_type: for_type.0.clone(),
                for_generics: vec![],
                methods: fndefs,
                method_bodies: vec![],
            });
        }
        Node::MacroDef { name, params, body, .. } => {
            let body_mir = lower_block_to_function(ctx, body);
            ctx.emit(MirInst::MacroDef {
                name: name.clone(),
                params: params.clone(),
                body: Box::new(body_mir),
            });
        }

        // ── TEA 定义 ──
        Node::ModelDef { name, fields, .. } => {
            let flds: Vec<crate::common::StructField> = fields
                .iter()
                .map(|(n, t)| crate::common::StructField {
                    name: n.clone(),
                    type_hint: t.0.clone(),
                })
                .collect();
            ctx.emit(MirInst::ModelDef {
                name: name.clone(),
                fields: flds,
            });
        }
        Node::MsgDef { name, variants, .. } => {
            let vars: Vec<crate::common::MsgVariant> = variants
                .iter()
                .map(|v| crate::common::MsgVariant {
                    name: v.name.clone(),
                    payload_type: v.payload.as_ref().map(|p| p.0.clone()),
                })
                .collect();
            ctx.emit(MirInst::MsgDef {
                name: name.clone(),
                variants: vars,
            });
        }
        Node::UpdateDef { name, params, body, .. } => {
            let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            let body_mir = lower_block_to_function(ctx, body);
            ctx.emit(MirInst::UpdateDef {
                name: name.clone(),
                params: param_names,
                body: Box::new(body_mir),
            });
        }
        Node::AppDef {
            name,
            model,
            msg,
            init,
            update_params,
            update,
            view_params,
            view,
            ..
        } => {
            // v0.104: update/view 的体块 + 形参名 —— 形参来自 witness_to_fcfg
            // 拆开的 Closure 节点（运行期 h_app_def 按位置绑定 MirFunction.params）。
            let init_mir = lower_block_to_function(ctx, init);
            let mut update_mir = lower_body_function_with_return(ctx, update);
            update_mir.params = update_params.iter().map(|p| p.name.clone()).collect();
            let mut view_mir = lower_body_function_with_return(ctx, view);
            view_mir.params = view_params.iter().map(|p| p.name.clone()).collect();
            ctx.emit(MirInst::AppDef {
                name: name.clone(),
                model_name: model.clone(),
                msg_name: msg.clone(),
                init_mir: Box::new(init_mir),
                update_mir: Box::new(update_mir),
                view_mir: Box::new(view_mir),
            });
        }
        Node::Export { names, decl, .. } => {
            lower_block(ctx, decl);
            for n in names {
                ctx.emit(MirInst::ExportMark(n.clone()));
            }
        }
        Node::Parallel { body, .. } => {
            let body_mir = lower_block_to_function(ctx, body);
            ctx.emit(MirInst::Parallel {
                body: Box::new(body_mir),
            });
        }
        // ── v0.103: 可观测性块 ──
        Node::Observe { config, body, .. } => {
            let body_mir = lower_block_to_function(ctx, body);
            ctx.emit(MirInst::Observe {
                config: config.clone(),
                body: Box::new(body_mir),
            });
        }
        Node::Span { name, tags, body, .. } => {
            let body_mir = lower_block_to_function(ctx, body);
            ctx.emit(MirInst::Span {
                name: name.clone(),
                tags: tags.clone(),
                body: Box::new(body_mir),
            });
        }
        // ── v0.103: 命名 section 声明 ──
        Node::PromptSection { name, body, .. } => {
            let body_mir = lower_block_to_function(ctx, body);
            ctx.emit(MirInst::PromptSection {
                name: name.clone(),
                body: Box::new(body_mir),
            });
        }
        Node::DocumentSection { name, body, .. } => {
            let body_mir = lower_block_to_function(ctx, body);
            ctx.emit(MirInst::DocumentSection {
                name: name.clone(),
                body: Box::new(body_mir),
            });
        }
        // ── v0.102: 声明式范式 ──
        Node::RelDef { name, clauses, .. } => {
            ctx.emit(MirInst::RelDef {
                name: name.clone(),
                clauses: clauses.clone(),
            });
        }
        Node::Solve { limit, query_vars, anon_vars, goal, .. } => {
            let goal_mir = lower_block_to_function(ctx, goal);
            let dst = ctx.alloc_reg();
            ctx.emit(MirInst::Solve {
                dst,
                limit: *limit,
                query_vars: query_vars.clone(),
                anon_vars: anon_vars.clone(),
                goal: Box::new(goal_mir),
            });
        }

        // ── 编排 ──
        Node::Orchestrate { input_var, result_var, .. } => {
            // FCFG OrchestrateKind → MirOrchestrateKind 转换需要 agent 定义，
            // 桥接层暂用 Sequential 占位。
            ctx.emit(MirInst::Orchestrate {
                input_var: input_var.clone(),
                result_var: result_var.clone(),
                kind: Box::new(super::orchestrate::MirOrchestrateKind::Sequential { agents: vec![] }),
            });
        }

        // ── 配置块 ──
        Node::WithConfig { bindings, body, .. } => {
            let body_mir = lower_block_to_function(ctx, body);
            ctx.emit(MirInst::WithConfig {
                bindings: bindings.clone(),
                body: Box::new(body_mir),
                jit: false,
            });
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
    // 避开块内节点预分配的寄存器（witness_to_fcfg 全局计数器分配）
    sub.next_reg = max_reg_in_nodes(&block.nodes) + 1;
    lower_block(&mut sub, block);
    sub.finish()
}

/// 将块降维为带尾部 Return 的 MirFunction（闭包/task/match-arm 体约定 —
/// 镜像 emit_closure_mir / emit_fn_def_w：body 末尾必须 Return 结果寄存器，
/// 否则 run_mir 调用闭包时返回 Nil）。Handle body/handler 不走此路径。
fn lower_body_function_with_return(ctx: &mut EmitContext, block: &Block<()>) -> super::MirFunction {
    let mut func = lower_block_to_function(ctx, block);
    if func.body.is_empty() || !matches!(func.body.last(), Some(MirInst::Return(_))) {
        let result_reg = block.result.unwrap_or_else(|| func.n_regs.saturating_sub(1));
        func.body.push(MirInst::Return(Some(result_reg)));
    }
    func
}

/// 降维 Match 表达式。
fn lower_match(ctx: &mut EmitContext, scrutinee: Reg, dst: Reg, arms: &[MatchArm<()>]) {
    // 镜像 emit_match_w：单条 MatchExpr，arm body 为嵌套 MirFunction
    //（末尾带 Return）。output_reg 统一为 dst — 所有 arm 写同一寄存器，
    // 消费者（let/Define/嵌套表达式）读 dst（与 inst.dst() 约定一致）。
    let mir_arms: Vec<super::MatchArmInst> = arms.iter()
        .map(|arm| {
            let pat_str = fcfg_pattern_to_string(&arm.pattern);
            let mut body_fn = lower_block_to_function(ctx, &arm.body);
            // 镜像 emit_match_arm_w：body 末尾补 Return（结果寄存器）
            if body_fn.body.is_empty() || !matches!(body_fn.body.last(), Some(MirInst::Return(_))) {
                let result_reg = arm.body.result.unwrap_or(0);
                body_fn.body.push(MirInst::Return(Some(result_reg)));
            }
            // v0.104.3: 守卫降维为独立 MirFunction（在模式绑定之后由
            // `h_match_expr` 调用）—— 与 body 同规则。
            let guard_fn = arm.guard.as_ref().map(|g| {
                let mut gf = lower_block_to_function(ctx, g);
                if gf.body.is_empty() || !matches!(gf.body.last(), Some(MirInst::Return(_))) {
                    let r = g.result.unwrap_or(0);
                    gf.body.push(MirInst::Return(Some(r)));
                }
                Box::new(gf)
            });
            (pat_str, guard_fn, Box::new(body_fn), dst)
        })
        .collect();
    let _ = ctx.alloc_reg(); // 镜像 emit_match_w：结果寄存器槽位保留
    ctx.emit(MirInst::MatchExpr { val: scrutinee, arms: mir_arms });
}

/// fcfg::Pattern → 字符串（镜像 lower.rs::pattern_to_string 的运行时
/// 匹配器格式：`int:42` / `list:vector:[a,..rest]` / `dict:{k:v,..}` 前缀式）。
fn fcfg_pattern_to_string(pattern: &Pattern) -> String {
    match pattern {
        Pattern::Wildcard => "_".to_string(),
        Pattern::Variable(name) => name.clone(),
        Pattern::Literal(lit) => match lit {
            crate::common::Literal::String(s, _) => format!("str:{}", s),
            crate::common::Literal::Char(c, _) => format!("char:{}", c),
            crate::common::Literal::Int(i, _) => format!("int:{}", i),
            crate::common::Literal::Float(f, _) => format!("float:{}", f),
            crate::common::Literal::BigInt(n, _) => format!("bigint:{}", n),
            crate::common::Literal::Bool(v, _) => format!("bool:{}", v),
            crate::common::Literal::Nil(_) => "nil".to_string(),
        },
        Pattern::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(fcfg_pattern_to_string).collect();
            format!("tuple:({})", parts.join(","))
        }
        // fcfg 的 List(Vec) = 元素列表（expr Pattern 无对应 — vector 语义）
        Pattern::List(items) => {
            let parts: Vec<String> = items.iter().map(fcfg_pattern_to_string).collect();
            format!("list:vector:[{}]", parts.join(","))
        }
        Pattern::ListVec { head, tail } => {
            let parts: Vec<String> = head.iter().map(fcfg_pattern_to_string).collect();
            match tail {
                Some(t) => format!("list:vector:[{},..{}]", parts.join(","), fcfg_pattern_to_string(t)),
                None => format!("list:vector:[{}]", parts.join(",")),
            }
        }
        Pattern::Dict(entries) => {
            let fields: Vec<String> = entries
                .iter()
                .map(|(k, v)| format!("{}:{}", k, fcfg_pattern_to_string(v)))
                .collect();
            format!("dict:{{{}}}", fields.join(","))
        }
        Pattern::TypeAscription(inner, ty) => {
            format!("{}:{}", ty.0, fcfg_pattern_to_string(inner))
        }
    }
}

/// Literal → Value 转换。
fn literal_to_value(lit: &crate::common::Literal) -> Value {
    match lit {
        crate::common::Literal::Int(n, _) => Value::Int(*n),
        crate::common::Literal::Float(f, _) => Value::Float(*f),
        crate::common::Literal::BigInt(n, _) => Value::BigInt(n.clone()),
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

    const S: Span = Span { line: 0, column: 0 };

    #[test]
    fn lower_literal_node() {
        let nodes = vec![Node::Literal {
            reg: 0,
            value: Literal::Int(42, Span::default()),
            span: S,
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
            span: S,
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
        let nodes = vec![Node::If {
            cond: 0,
            then: Block {
                nodes: vec![Node::Literal {
                    reg: 1,
                    value: Literal::Int(1, Span::default()),
                    span: S,
                    meta: (),
                }],
                result: Some(1),
            },
            else_: Some(Block {
                nodes: vec![Node::Literal {
                    reg: 2,
                    value: Literal::Int(2, Span::default()),
                    span: S,
                    meta: (),
                }],
                result: Some(2),
            }),
            // v0.104.2: if 的结果寄存器由 witness_to_fcfg 预分配（两分支 Copy 到它）
            dst: 3,
            span: S,
            meta: (),
        }];
        let (insts, n_regs) = lower_fcfg(&nodes);
        // 新形状（镜像 emit 路径）：JumpIfNot + Const + Copy + Jump + Const + Copy
        assert_eq!(insts.len(), 6, "expected 6 instructions: {:?}", insts);
        assert!(matches!(&insts[0], MirInst::JumpIfNot(0, _)));
        assert!(matches!(&insts[1], MirInst::Const(1, Value::Int(1))));
        assert!(matches!(&insts[2], MirInst::Copy(3, 1)));
        assert!(matches!(&insts[3], MirInst::Jump(_)));
        assert!(matches!(&insts[4], MirInst::Const(2, Value::Int(2))));
        assert!(matches!(&insts[5], MirInst::Copy(3, 2)));
        let _ = n_regs;
    }

    #[test]
    fn lower_sequence() {
        let nodes = vec![Node::Sequence {
            nodes: vec![
                Node::Literal {
                    reg: 0,
                    value: Literal::Int(1, Span::default()),
                    span: S,
                    meta: (),
                },
                Node::Literal {
                    reg: 1,
                    value: Literal::Int(2, Span::default()),
                    span: S,
                    meta: (),
                },
            ],
            span: S,
            meta: (),
        }];
        let (insts, _) = lower_fcfg(&nodes);
        assert_eq!(insts.len(), 2);
    }
}
