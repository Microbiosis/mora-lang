//! v0.89: EHIR → Core 降维 — 9 层架构桥接第 1 段。
//!
//! 将 Node<TypeInfo>（带类型的结构化 CFG）降维为 CoreInst（SSA 基元）。
//!
//! 降维内容：
//! - FnDef → ClosureCreate + EnvStore（函数即闭包）
//! - Handle → EffectInstall + body + EffectRestore
//! - Perform → EffectPerform（EffectLabel 从 TypeInfo.effects 提取）
//! - Match → EnumMatch（模式匹配降维为枚举匹配）
//! - Orchestrate → PregelGraph/MoAPipeline/MoERouter（编排降维）
//! - Let/Assign → EnvStore/EnvMutate（环境操作）
//! - 声明类（TypeAlias/EnumDef/StructDef/TraitDef/ImplDef）→ 编译期消失
//! - TEA（ModelDef/MsgDef/UpdateDef/AppDef）→ 编译期注册

use crate::mir::core::{
    BlockId, CoreBlock, CoreFunction, CoreInst, CoreTerminator, EffectLabel, EnumMatchArm,
};
use crate::mir::fcfg::{Ehir, EhirBlock, Node};
use crate::mir::effect::EffectRow;
use crate::typeck::Type;
use crate::value::Value;

/// 将 EHIR 节点列表降维为 CoreFunction。
pub fn ehir_to_core(nodes: &[Ehir], params: Vec<(String, Type)>, effects: EffectRow) -> CoreFunction {
    let mut ctx = CoreContext::new();
    for node in nodes {
        lower_node(&mut ctx, node);
    }
    ctx.finish(params, effects)
}

/// Core 降维上下文。
struct CoreContext {
    next_reg: usize,
    blocks: Vec<CoreBlock>,
    current_insts: Vec<CoreInst>,
    next_block_id: BlockId,
}

impl CoreContext {
    fn new() -> Self {
        Self {
            next_reg: 0,
            blocks: Vec::new(),
            current_insts: Vec::new(),
            next_block_id: 0,
        }
    }

    fn alloc_reg(&mut self) -> usize {
        let r = self.next_reg;
        self.next_reg += 1;
        r
    }

    fn emit(&mut self, inst: CoreInst) {
        self.current_insts.push(inst);
    }

    fn finish(mut self, params: Vec<(String, Type)>, effects: EffectRow) -> CoreFunction {
        self.flush_block(CoreTerminator::Return(None));
        CoreFunction {
            params,
            blocks: self.blocks,
            entry: 0,
            effects,
            n_regs: self.next_reg,
        }
    }

    fn flush_block(&mut self, terminator: CoreTerminator) {
        let id = self.next_block_id;
        self.next_block_id += 1;
        self.blocks.push(CoreBlock {
            id,
            insts: std::mem::take(&mut self.current_insts),
            terminator,
        });
    }
}

/// 降维单个 EHIR 节点。
fn lower_node(ctx: &mut CoreContext, node: &Ehir) {
    match node {
        // ── 值产生 ──
        Node::Literal { reg, value, .. } => {
            ctx.emit(CoreInst::Const(*reg, literal_to_value(value)));
        }
        Node::Variable { reg, name, .. } => {
            ctx.emit(CoreInst::EnvLoad(*reg, name.clone()));
        }
        Node::BinaryOp { dst, lhs, op, rhs, .. } => {
            ctx.emit(CoreInst::BinaryOp(*dst, *lhs, op.clone(), *rhs));
        }
        // Or/And → Core 层降维为短路求值的控制流（Branch + 常量合并）
        Node::Or { dst, lhs, rhs, .. } => {
            // 简化：非短路直接 BinaryOp（NotEqual 语义与 emit_or_w 一致）
            ctx.emit(CoreInst::BinaryOp(*dst, *lhs, crate::common::BinaryOp::NotEqual, *rhs));
        }
        Node::And { dst, lhs, rhs, .. } => {
            ctx.emit(CoreInst::BinaryOp(*dst, *lhs, crate::common::BinaryOp::Equal, *rhs));
        }
        // DynTrait/Prompt → Core 层降维为透传调用
        Node::DynTrait { dst, src, .. } => {
            ctx.emit(CoreInst::Copy(*dst, *src));
        }
        Node::Prompt { dst, parts, .. } => {
            // Prompt 在 Core 层表示为 Call("prompt", parts)
            let prompt_callee = ctx.alloc_reg();
            ctx.emit(CoreInst::Call(*dst, prompt_callee, parts.clone()));
        }
        Node::ClosureExpr { dst, params, body, .. } => {
            let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            let core_body = lower_block_to_core(ctx, body);
            ctx.emit(CoreInst::ClosureCreate {
                dst: *dst,
                params: param_names,
                captures: vec![],
                body: core_body,
            });
        }
        Node::Call { dst, callee, callee_name, args, .. } => {
            // 已知函数名 → 直接 Call，否则 → ClosureCall
            if callee_name.is_some() {
                ctx.emit(CoreInst::Call(*dst, *callee, args.clone()));
            } else {
                ctx.emit(CoreInst::ClosureCall(*dst, *callee, args.clone()));
            }
        }
        Node::MethodCall { dst, receiver, method, args, .. } => {
            // 方法调用降维为函数调用：callee = receiver, args = [receiver, ...args]
            let mut full_args = vec![*receiver];
            full_args.extend_from_slice(args);
            ctx.emit(CoreInst::Call(*dst, *receiver, full_args));
            let _ = method; // 方法名已在 EHIR 阶段解析
        }
        Node::ListLit { dst, items, .. } => {
            ctx.emit(CoreInst::ListLit(*dst, items.clone()));
        }
        Node::DictLit { dst, entries, .. } => {
            ctx.emit(CoreInst::DictLit(*dst, entries.clone()));
        }
        Node::Index { dst, obj, idx, .. } => {
            ctx.emit(CoreInst::Index(*dst, *obj, *idx));
        }

        // ── 控制流 ──
        Node::If { cond, then, else_, .. } => {
            // 降维为 Branch + 两个基本块
            let then_id = ctx.next_block_id + 1;
            let else_id = if else_.is_some() { ctx.next_block_id + 2 } else { ctx.next_block_id + 3 };
            let end_id = ctx.next_block_id + 3;

            ctx.emit(CoreInst::Branch {
                cond: *cond,
                true_bb: then_id,
                false_bb: else_id,
            });
            ctx.flush_block(CoreTerminator::Branch {
                cond: *cond,
                true_bb: then_id,
                false_bb: else_id,
            });

            // then block
            lower_block(ctx, then);
            ctx.flush_block(CoreTerminator::Jump(end_id));

            // else block
            if let Some(else_block) = else_ {
                lower_block(ctx, else_block);
            }
            ctx.flush_block(CoreTerminator::Jump(end_id));
        }
        Node::While { cond, body, .. } => {
            let cond_id = ctx.next_block_id;
            let body_id = ctx.next_block_id + 1;
            let end_id = ctx.next_block_id + 2;

            // cond block
            lower_block(ctx, cond);
            let cond_reg = cond.result.unwrap_or(0);
            ctx.emit(CoreInst::Branch {
                cond: cond_reg,
                true_bb: body_id,
                false_bb: end_id,
            });
            ctx.flush_block(CoreTerminator::Branch {
                cond: cond_reg,
                true_bb: body_id,
                false_bb: end_id,
            });

            // body block
            lower_block(ctx, body);
            ctx.emit(CoreInst::Jump(cond_id));
            ctx.flush_block(CoreTerminator::Jump(cond_id));
        }
        Node::For { var, iter, body, .. } => {
            // index-based loop
            let idx_reg = ctx.alloc_reg();
            ctx.emit(CoreInst::Const(idx_reg, Value::Int(0)));
            let len_reg = ctx.alloc_reg();
            ctx.emit(CoreInst::Call(len_reg, *iter, vec![*iter]));

            let loop_start = ctx.next_block_id;
            let body_id = ctx.next_block_id + 1;
            let end_id = ctx.next_block_id + 2;

            let cond_reg = ctx.alloc_reg();
            ctx.emit(CoreInst::BinaryOp(
                cond_reg, idx_reg,
                crate::common::BinaryOp::GreaterEqual, len_reg,
            ));
            ctx.emit(CoreInst::Branch {
                cond: cond_reg,
                true_bb: end_id,
                false_bb: body_id,
            });
            ctx.flush_block(CoreTerminator::Branch {
                cond: cond_reg,
                true_bb: end_id,
                false_bb: body_id,
            });

            // body
            let val_reg = ctx.alloc_reg();
            ctx.emit(CoreInst::Index(val_reg, *iter, idx_reg));
            ctx.emit(CoreInst::EnvStore(var.clone(), val_reg));
            lower_block(ctx, body);
            let one = ctx.alloc_reg();
            ctx.emit(CoreInst::Const(one, Value::Int(1)));
            ctx.emit(CoreInst::BinaryOp(
                idx_reg, idx_reg,
                crate::common::BinaryOp::Add, one,
            ));
            ctx.emit(CoreInst::Jump(loop_start));
            ctx.flush_block(CoreTerminator::Jump(loop_start));
        }
        Node::Match { dst, scrutinee, arms, .. } => {
            // 降维为 EnumMatch（dst 作为统一结果寄存器透传）
            let core_arms: Vec<EnumMatchArm> = arms
                .iter()
                .map(|arm| EnumMatchArm {
                    variant: format!("{:?}", arm.pattern),
                    bindings: vec![],
                    body: lower_block_to_core(ctx, &arm.body),
                })
                .collect();
            ctx.emit(CoreInst::EnumMatch {
                scrutinee: *scrutinee,
                arms: core_arms,
            });
            let _ = dst;
        }
        Node::Return { value, .. } => {
            ctx.flush_block(CoreTerminator::Return(*value));
        }
        Node::Break { .. } | Node::Continue { .. } => {
            // Break/Continue 在 Core 层降维为 Jump（目标由循环结构决定）
            ctx.emit(CoreInst::Jump(0)); // placeholder
        }

        // ── 绑定 ──
        Node::Let { name, value, body, .. } => {
            ctx.emit(CoreInst::EnvStore(name.clone(), *value));
            lower_block(ctx, body);
        }
        Node::Assign { name, value, .. } => {
            ctx.emit(CoreInst::EnvMutate(name.clone(), *value));
        }
        Node::IndexAssign { obj, idx, value, .. } => {
            ctx.emit(CoreInst::IndexAssign(*obj, *idx, *value));
        }

        // ── 闭包/函数 ──
        Node::FnDef { name, params, body, .. } => {
            let param_names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
            let captures = vec![]; // 捕获变量由 EHIR 分析确定
            let core_body = lower_block_to_core(ctx, body);
            let closure_reg = ctx.alloc_reg();
            ctx.emit(CoreInst::ClosureCreate {
                dst: closure_reg,
                params: param_names,
                captures,
                body: core_body,
            });
            ctx.emit(CoreInst::EnvStore(name.clone(), closure_reg));
        }

        // ── 效果 ──
        Node::Perform { dst, effect, args, .. } => {
            ctx.emit(CoreInst::EffectPerform {
                dst: *dst,
                label: EffectLabel::from_name(effect),
                args: args.clone(),
            });
        }
        Node::Handle { effect, body, handler, .. } => {
            let handler_reg = ctx.alloc_reg();
            // handler 降维为闭包
            let handler_body = lower_block_to_core(ctx, handler);
            ctx.emit(CoreInst::ClosureCreate {
                dst: handler_reg,
                params: vec!["__arg0".to_string()],
                captures: vec![],
                body: handler_body,
            });
            ctx.emit(CoreInst::EffectInstall {
                label: EffectLabel::from_name(effect),
                handler: handler_reg,
            });
            lower_block(ctx, body);
            ctx.emit(CoreInst::EffectRestore {
                label: EffectLabel::from_name(effect),
            });
        }

        // ── 声明类 → 编译期消失 ──
        Node::TypeAlias { .. }
        | Node::EnumDef { .. }
        | Node::StructDef { .. }
        | Node::TraitDef { .. }
        | Node::ImplDef { .. }
        | Node::Import { .. }
        | Node::MacroDef { .. } => {
            // 编译期注册，Core 层无操作
        }

        // ── TEA → 编译期注册 ──
        Node::ModelDef { .. }
        | Node::MsgDef { .. }
        | Node::UpdateDef { .. }
        | Node::AppDef { .. } => {
            // TEA 定义在 EHIR 阶段已注册到 env，Core 层无操作
        }

        // ── v0.103: export → Core 层透传内部声明（导出标记在执行层）──
        Node::Export { decl, .. } => {
            lower_block(ctx, decl);
        }
        // ── v0.103: parallel → Core 层透传 body（并发在执行层）──
        Node::Parallel { body, .. } => {
            lower_block(ctx, body);
        }
        // ── v0.103: 可观测性块 → Core 层透传 body（span 记录在执行层）──
        Node::Observe { body, .. } | Node::Span { body, .. } => {
            lower_block(ctx, body);
        }
        // ── v0.103: 命名 section 声明 → 与 TEA 定义同为编译期注册 ──
        Node::PromptSection { body, .. } | Node::DocumentSection { body, .. } => {
            // section 的构建在 MIR 执行层（h_prompt_section 绑定值到 env）；
            // Core 层降维 body 内的表达式
            lower_block(ctx, body);
        }
        // ── v0.102: 声明式范式 ──
        Node::RelDef { .. } => {
            // 关系定义在 emit 阶段注册到 env，Core 层无操作（同 TEA 定义）
        }
        Node::Solve { goal, .. } => {
            // 目标构建体降维处理；solve 的搜索语义在 MIR 执行层
            //（同 WithConfig 的 body 透传模式）
            lower_block(ctx, goal);
        }

        // ── 编排 → CMIR 层处理，Core 层透传 ──
        Node::Orchestrate { .. } => {
            // 编排在 CMIR 层降维，Core 层不处理
        }

        // ── 配置块 → 透传 body ──
        Node::WithConfig { body, .. } => {
            lower_block(ctx, body);
        }

        // ── 元编程 → 编译期展开 ──
        Node::Quasiquote { dst, segments, .. } => {
            let resolved: Vec<crate::mir::QuasiquoteSegment> = segments
                .iter()
                .map(|seg| match seg {
                    crate::mir::fcfg::QuasiquoteSegment::Quote(s) => {
                        crate::mir::QuasiquoteSegment::Quote(s.clone())
                    }
                    crate::mir::fcfg::QuasiquoteSegment::Unquote(r) => {
                        crate::mir::QuasiquoteSegment::Unquote(*r)
                    }
                    crate::mir::fcfg::QuasiquoteSegment::UnquoteSplice(r) => {
                        crate::mir::QuasiquoteSegment::UnquoteSplice(*r)
                    }
                })
                .collect();
            ctx.emit(CoreInst::Const(*dst, Value::Nil)); // placeholder
            let _ = resolved;
        }

        // ── 序列 ──
        Node::Sequence { nodes, .. } => {
            for n in nodes {
                lower_node(ctx, n);
            }
        }

        // ── 表达式语句 ──
        Node::Expr { reg, .. } => {
            ctx.emit(CoreInst::Expr(*reg));
        }
    }
}

fn lower_block(ctx: &mut CoreContext, block: &EhirBlock) {
    for node in &block.nodes {
        lower_node(ctx, node);
    }
}

fn lower_block_to_core(_ctx: &mut CoreContext, block: &EhirBlock) -> CoreBlock {
    let mut sub = CoreContext::new();
    for node in &block.nodes {
        lower_node(&mut sub, node);
    }
    sub.flush_block(CoreTerminator::Return(block.result));
    sub.blocks.into_iter().next().unwrap_or(CoreBlock {
        id: 0,
        insts: vec![],
        terminator: CoreTerminator::Return(None),
    })
}

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
    use crate::mir::effect::EffectRow;
    use crate::mir::fcfg::{Block, TypeInfo};
    use crate::typeck::Type;

    const S: Span = Span { line: 1, column: 0 };

    fn lit_int_ehir(n: i64, reg: usize) -> Ehir {
        Node::Literal {
            reg,
            value: Literal::Int(n, S),
            span: S,
            meta: TypeInfo::new(Type::Int, EffectRow::Empty, S),
        }
    }

    #[test]
    fn ehir_literal_to_core_const() {
        let nodes = vec![lit_int_ehir(42, 0)];
        let func = ehir_to_core(&nodes, vec![], EffectRow::Empty);
        assert_eq!(func.blocks.len(), 1);
        assert_eq!(func.blocks[0].insts.len(), 1);
        match &func.blocks[0].insts[0] {
            CoreInst::Const(r, Value::Int(v)) => {
                assert_eq!(*r, 0);
                assert_eq!(*v, 42);
            }
            _ => panic!("expected Const"),
        }
    }

    #[test]
    fn ehir_binary_op() {
        let nodes = vec![Node::BinaryOp {
            dst: 2,
            lhs: 0,
            op: crate::common::BinaryOp::Add,
            rhs: 1,
            span: S,
            meta: TypeInfo::new(Type::Int, EffectRow::Empty, S),
        }];
        let func = ehir_to_core(&nodes, vec![], EffectRow::Empty);
        assert_eq!(func.blocks[0].insts.len(), 1);
        assert!(matches!(&func.blocks[0].insts[0], CoreInst::BinaryOp(2, 0, _, 1)));
    }

    #[test]
    fn ehir_let_binding() {
        let nodes = vec![Node::Let {
            name: "x".to_string(),
            type_ann: None,
            value: 0,
            body: Block {
                nodes: vec![lit_int_ehir(1, 1)],
                result: Some(1),
            },
            span: S,
            meta: TypeInfo::unknown(S),
        }];
        let func = ehir_to_core(&nodes, vec![], EffectRow::Empty);
        assert!(func.blocks[0].insts.len() >= 2); // EnvStore + Const
    }
}
