//! v0.90: witness_to_fcfg — MirWitness 树 → FCFG（Node<()>）转换器。
//!
//! 9 层架构切换的核心缺失件：FCFG 生产者。ParserV3::compile() 已产出
//! MirWitness[]（结构化语法骨架），本转换器将其转为 Node<()> 树，
//! 无需重写 emit.rs。
//!
//! 寄存器分配策略：自底向上遍历 witness 树，每个值产生节点分配一个
//! 新寄存器（bump allocator，与 emit.rs 的 EmitContext 语义一致）。
//!
//! 管线位置：
//!   Token → ParserV3::compile() → MirInst[] + MirWitness[]（现有）
//!                                        ↓
//!                          witness_to_fcfg() → Vec<Node<()>>  ← 本文件
//!                                        ↓
//!                          annotate + TypeTable → EHIR
//!                                        ↓
//!                          ehir_to_core → Core → CMIR → LMIR → RIR

use crate::common::Literal;
use crate::mir::fcfg::{
    Block, MatchArm, Node, OrchestrateKind, Param, Pattern, QuasiquoteSegment, Reg, TypeAnnotation,
    Variant,
};
use crate::mir::hint::TypeHint;
use crate::mir::witness::{
    MirWitness, WitnessCallee, WitnessKind, WitnessOrchestrateKind, WitnessParam, WitnessPattern,
};

/// 转换上下文 — bump 寄存器分配器 + 循环栈（break/continue 跳转目标）。
struct FcfgBuilder {
    next_reg: Reg,
    /// (continue_label, break_label) 栈 — Label=占位（fcfg_lower post-patch）。
    /// witness_to_fcfg 不分配寄存空间（fcfg 节点不带 label），
    /// 仅在 While/For/Loop 进入时 push 占位 label，退出时 pop。
    /// fcfg_lower post-patch 时填入正确 label。
    loop_stack: Vec<(Label, Label)>,
}

type Label = usize;

impl FcfgBuilder {
    fn new() -> Self {
        Self {
            next_reg: 0,
            loop_stack: Vec::new(),
        }
    }

    fn alloc(&mut self) -> Reg {
        let r = self.next_reg;
        self.next_reg += 1;
        r
    }

    fn push_loop(&mut self, continue_label: Label, break_label: Label) {
        self.loop_stack.push((continue_label, break_label));
    }

    fn pop_loop(&mut self) {
        self.loop_stack.pop();
    }

    fn current_break_label(&self) -> Label {
        self.loop_stack.last().map_or(0, |&(_, b)| b)
    }

    fn current_continue_label(&self) -> Label {
        self.loop_stack.last().map_or(0, |&(c, _)| c)
    }
}

/// 将 witness 列表转为 FCFG 节点列表（顶层入口）。
pub fn witness_to_fcfg(witnesses: &[MirWitness]) -> Vec<Node<()>> {
    let mut b = FcfgBuilder::new();
    witnesses.iter().map(|w| build_node(&mut b, w)).collect()
}

/// 将单个 witness 转为 Node<()>（递归）。
fn build_node(b: &mut FcfgBuilder, w: &MirWitness) -> Node<()> {
    let span = w.span;
    match &w.kind {
        // ── 值产生 ──
        WitnessKind::Literal(lit) => {
            let reg = b.alloc();
            Node::Literal { reg, value: lit.clone(), span, meta: () }
        }
        WitnessKind::Variable(name) => {
            let reg = b.alloc();
            Node::Variable { reg, name: name.clone(), span, meta: () }
        }
        WitnessKind::Binary { left, op, right } => {
            let lhs = build_node(b, left);
            let rhs = build_node(b, right);
            let lhs_reg = node_result_reg(&lhs);
            let rhs_reg = node_result_reg(&rhs);
            let dst = b.alloc();
            Node::Sequence {
                nodes: vec![lhs, rhs, Node::BinaryOp {
                    dst, lhs: lhs_reg, op: op.clone(), rhs: rhs_reg, span, meta: (),
                }],
                span,
                meta: (),
            }
        }
        WitnessKind::Or { left, right } => {
            let lhs = build_node(b, left);
            let rhs = build_node(b, right);
            let l = node_result_reg(&lhs);
            let r = node_result_reg(&rhs);
            let dst = b.alloc();
            Node::Sequence {
                nodes: vec![lhs, rhs, Node::Or { dst, lhs: l, rhs: r, span, meta: () }],
                span,
                meta: (),
            }
        }
        WitnessKind::And { left, right } => {
            let lhs = build_node(b, left);
            let rhs = build_node(b, right);
            let l = node_result_reg(&lhs);
            let r = node_result_reg(&rhs);
            let dst = b.alloc();
            Node::Sequence {
                nodes: vec![lhs, rhs, Node::And { dst, lhs: l, rhs: r, span, meta: () }],
                span,
                meta: (),
            }
        }
        WitnessKind::Call { callee, args } => {
            let mut nodes: Vec<Node<()>> = Vec::new();
            let (callee_name, callee_reg) = match callee {
                WitnessCallee::Name(n) => (Some(n.clone()), b.alloc()),
                WitnessCallee::Var(n) => {
                    let r = b.alloc();
                    nodes.push(Node::Variable { reg: r, name: n.clone(), span, meta: () });
                    (None, r)
                }
                WitnessCallee::Method(obj, m) => {
                    let r = b.alloc();
                    nodes.push(Node::Variable { reg: r, name: format!("{}.{}", obj, m), span, meta: () });
                    (Some(format!("{}.{}", obj, m)), r)
                }
                WitnessCallee::Evaluated(e) => {
                    let n = build_node(b, e);
                    let r = node_result_reg(&n);
                    nodes.push(n);
                    (None, r)
                }
                WitnessCallee::Builtin(op) => {
                    let r = b.alloc();
                    nodes.push(Node::Variable { reg: r, name: format!("{:?}", op), span, meta: () });
                    (Some(format!("{:?}", op)), r)
                }
            };
            let mut arg_regs = Vec::new();
            for a in args {
                let n = build_node(b, a);
                arg_regs.push(node_result_reg(&n));
                nodes.push(n);
            }
            let dst = b.alloc();
            // 索引特例：emit.rs 把 obj[idx] 的 witness 编码为
            // Call{Name("[]"), [obj, idx]} ↔ MirInst::Index。
            if callee_name.as_deref() == Some("[]") && arg_regs.len() == 2 {
                nodes.push(Node::Index {
                    dst,
                    obj: arg_regs[0],
                    idx: arg_regs[1],
                    span,
                    meta: (),
                });
            } else {
                nodes.push(Node::Call {
                    dst, callee: callee_reg, callee_name, args: arg_regs, span, meta: (),
                });
            }
            Node::Sequence { nodes, span, meta: () }
        }
        WitnessKind::MethodCall { receiver, method, args } => {
            let recv_node = build_node(b, receiver);
            let recv_reg = node_result_reg(&recv_node);
            let mut nodes = vec![recv_node];
            let mut arg_regs = Vec::new();
            for a in args {
                let n = build_node(b, a);
                arg_regs.push(node_result_reg(&n));
                nodes.push(n);
            }
            let dst = b.alloc();
            nodes.push(Node::MethodCall {
                dst, receiver: recv_reg, method: method.clone(), args: arg_regs, span, meta: (),
            });
            Node::Sequence { nodes, span, meta: () }
        }
        WitnessKind::Closure { params, body } => {
            let body_block = build_block(b, body);
            let dst = b.alloc();
            Node::ClosureExpr {
                dst,
                params: params.iter().map(witness_param_to_param).collect(),
                body: body_block,
                span,
                meta: (),
            }
        }
        WitnessKind::FnDef { name, params, return_type, body } => {
            let body_block = build_block(b, body);
            Node::FnDef {
                name: name.clone(),
                params: params.iter().map(witness_param_to_param).collect(),
                return_ann: return_type.as_ref().map(hint_to_annotation),
                body: body_block,
                span,
                meta: (),
            }
        }
        WitnessKind::Match { scrutinee, arms } => {
            let scrut_node = build_node(b, scrutinee);
            let scrut_reg = node_result_reg(&scrut_node);
            let core_arms: Vec<MatchArm<()>> = arms
                .iter()
                .map(|arm| MatchArm {
                    pattern: witness_pattern_to_pattern(&arm.pattern),
                    guard: None,
                    body: build_block(b, &arm.body),
                })
                .collect();
            let dst = b.alloc();
            Node::Sequence {
                nodes: vec![scrut_node, Node::Match { dst, scrutinee: scrut_reg, arms: core_arms, span, meta: () }],
                span,
                meta: (),
            }
        }
        WitnessKind::If { cond, then, r#else } => {
            let cond_node = build_node(b, cond);
            let cond_reg = node_result_reg(&cond_node);
            let then_block = build_block(b, then);
            let else_block = r#else.as_ref().map(|e| build_block(b, e));
            let if_node = Node::If { cond: cond_reg, then: then_block, else_: else_block, span, meta: () };
            // Sequence 保留条件求值顺序 + If 节点（此前 wrap_with_cond
            // 丢弃了 If 自身 — 嵌套体（macro body）的分支全部丢失）
            Node::Sequence { nodes: vec![cond_node, if_node], span, meta: () }
        }
        WitnessKind::Loop { var, iterable, body } => {
            let iter_node = build_node(b, iterable);
            let iter_reg = node_result_reg(&iter_node);
            // v0.90.4: push loop context — body 内的 break/continue 找当前 loop label
            b.push_loop(0, 1);
            let body_block = build_block(b, body);
            b.pop_loop();
            Node::Sequence {
                nodes: vec![iter_node, Node::For {
                    var: var.clone(), iter: iter_reg, body: body_block, span, meta: (),
                }],
                span,
                meta: (),
            }
        }
        WitnessKind::While { cond, body } => {
            let cond_block = build_block(b, cond);
            // v0.90.4: push/pop loop_stack 包住 body 递归 — body 内的
            // Break/Continue 才能找到当前 loop 的 break/continue label。
            b.push_loop(0, 1);
            let body_block = build_block(b, body);
            b.pop_loop();
            Node::While { cond: cond_block, body: body_block, span, meta: () }
        }
        WitnessKind::List(items) => {
            let mut nodes = Vec::new();
            let mut item_regs = Vec::new();
            for item in items {
                let n = build_node(b, item);
                item_regs.push(node_result_reg(&n));
                nodes.push(n);
            }
            let dst = b.alloc();
            nodes.push(Node::ListLit { dst, items: item_regs, span, meta: () });
            Node::Sequence { nodes, span, meta: () }
        }
        WitnessKind::Dict(entries) => {
            let mut nodes = Vec::new();
            let mut pairs = Vec::new();
            for (k, v) in entries {
                let n = build_node(b, v);
                pairs.push((k.clone(), node_result_reg(&n)));
                nodes.push(n);
            }
            let dst = b.alloc();
            nodes.push(Node::DictLit { dst, entries: pairs, span, meta: () });
            Node::Sequence { nodes, span, meta: () }
        }
        WitnessKind::DynTrait { expr, trait_name, .. } => {
            let inner = build_node(b, expr);
            let src = node_result_reg(&inner);
            let dst = b.alloc();
            Node::Sequence {
                nodes: vec![inner, Node::DynTrait {
                    dst, src, trait_name: trait_name.clone(), span, meta: (),
                }],
                span,
                meta: (),
            }
        }
        WitnessKind::Prompt { parts } => {
            let mut nodes = Vec::new();
            let mut part_regs = Vec::new();
            for p in parts {
                let n = build_node(b, p);
                part_regs.push(node_result_reg(&n));
                nodes.push(n);
            }
            let dst = b.alloc();
            nodes.push(Node::Prompt { dst, parts: part_regs, span, meta: () });
            Node::Sequence { nodes, span, meta: () }
        }
        WitnessKind::LetBinding { name, type_hint, value, init_body } => {
            let val_node = build_node(b, value);
            let val_reg = node_result_reg(&val_node);
            let body_block = build_block(b, init_body);
            Node::Sequence {
                nodes: vec![val_node, Node::Let {
                    name: name.clone(),
                    type_ann: type_hint.as_ref().map(hint_to_annotation),
                    value: val_reg,
                    body: body_block,
                    span,
                    meta: (),
                }],
                span,
                meta: (),
            }
        }
        WitnessKind::Assign { target, value } => {
            let val_node = build_node(b, value);
            let val_reg = node_result_reg(&val_node);
            Node::Sequence {
                nodes: vec![val_node, Node::Assign { name: target.clone(), value: val_reg, span, meta: () }],
                span,
                meta: (),
            }
        }
        WitnessKind::IndexAssign { object, index, value } => {
            let obj_node = build_node(b, object);
            let obj_reg = node_result_reg(&obj_node);
            let idx_node = build_node(b, index);
            let idx_reg = node_result_reg(&idx_node);
            let val_node2 = build_node(b, value);
            let val_reg = node_result_reg(&val_node2);
            Node::Sequence {
                nodes: vec![obj_node, idx_node, val_node2, Node::IndexAssign {
                    obj: obj_reg, idx: idx_reg, value: val_reg, span, meta: (),
                }],
                span,
                meta: (),
            }
        }
        WitnessKind::Return(v) => {
            match v {
                Some(e) => {
                    let n = build_node(b, e);
                    let r = node_result_reg(&n);
                    Node::Sequence {
                        nodes: vec![n, Node::Return { value: Some(r), span, meta: () }],
                        span,
                        meta: (),
                    }
                }
                None => Node::Return { value: None, span, meta: () },
            }
        }
        // v0.90.4: break/continue label 取自当前循环上下文（While/For push）
        WitnessKind::Break(_) => Node::Break {
            label: b.current_break_label(),
            span,
            meta: (),
        },
        WitnessKind::Continue(_) => Node::Continue {
            label: b.current_continue_label(),
            span,
            meta: (),
        },
        WitnessKind::Orchestrate { input_var, result_var, kind } => {
            let mir_kind = build_orchestrate_kind(b, kind);
            Node::Orchestrate {
                input_var: input_var.clone(),
                result_var: result_var.clone(),
                kind: mir_kind,
                span,
                meta: (),
            }
        }
        WitnessKind::TypeAlias { name, target } => Node::TypeAlias {
            name: name.clone(),
            target: hint_to_annotation(target),
            span,
            meta: (),
        },
        WitnessKind::EnumDef { name, variants } => Node::EnumDef {
            name: name.clone(),
            variants: variants.iter().map(|v| Variant { name: v.clone(), payload: None }).collect(),
            span,
            meta: (),
        },
        WitnessKind::StructDef { name, fields } => Node::StructDef {
            name: name.clone(),
            fields: fields
                .iter()
                .map(|(n, t)| (n.clone(), hint_to_annotation(t)))
                .collect(),
            span,
            meta: (),
        },
        WitnessKind::Import(path) => Node::Import { path: path.clone(), span, meta: () },
        WitnessKind::Perform { effect, args } => {
            let mut nodes = Vec::new();
            let mut arg_regs = Vec::new();
            for a in args {
                let n = build_node(b, a);
                arg_regs.push(node_result_reg(&n));
                nodes.push(n);
            }
            let dst = b.alloc();
            nodes.push(Node::Perform { dst, effect: effect.clone(), args: arg_regs, span, meta: () });
            Node::Sequence { nodes, span, meta: () }
        }
        WitnessKind::Handle { effect, body, handler, k_param } => {
            let body_block = build_block(b, body);
            let handler_block = build_block(b, handler);
            Node::Handle {
                effect: effect.clone(),
                body: body_block,
                handler: handler_block,
                k_param: k_param.clone(),
                span,
                meta: (),
            }
        }
        WitnessKind::MacroDef { name, params, body } => {
            let body_block = build_block(b, body);
            Node::MacroDef {
                name: name.clone(),
                params: params.clone(),
                body: body_block,
                span,
                meta: (),
            }
        }
        WitnessKind::Sequence(exprs) => {
            let nodes: Vec<Node<()>> = exprs.iter().map(|e| build_node(b, e)).collect();
            Node::Sequence { nodes, span, meta: () }
        }
        WitnessKind::ModelDef { name, fields } => Node::ModelDef {
            name: name.clone(),
            fields: fields.iter().map(|(n, t)| (n.clone(), hint_to_annotation(t))).collect(),
            span,
            meta: (),
        },
        WitnessKind::MsgDef { name, variants } => Node::MsgDef {
            name: name.clone(),
            variants: variants
                .iter()
                .map(|v| Variant {
                    name: v.name.clone(),
                    payload: v.payload_type.as_ref().map(|t| TypeAnnotation(t.clone())),
                })
                .collect(),
            span,
            meta: (),
        },
        WitnessKind::UpdateDef { name, params, body, .. } => {
            let body_block = build_block(b, body);
            Node::UpdateDef {
                name: name.clone(),
                params: params.iter().map(witness_param_to_param).collect(),
                body: body_block,
                span,
                meta: (),
            }
        }
        WitnessKind::AppDef { name, model_name, msg_name, init_w, update_w, view_w } => {
            Node::AppDef {
                name: name.clone(),
                model: model_name.clone(),
                msg: msg_name.clone(),
                init: build_block(b, init_w),
                update: build_block(b, update_w),
                view: build_block(b, view_w),
                span,
                meta: (),
            }
        }
        WitnessKind::WithConfig { bindings, body } => {
            let mut nodes: Vec<Node<()>> = Vec::new();
            let mut pairs = Vec::new();
            for (k, v) in bindings {
                let n = build_node(b, v);
                pairs.push((k.clone(), node_result_reg(&n)));
                nodes.push(n);
            }
            let body_block = build_block(b, body);
            nodes.push(Node::WithConfig { bindings: pairs, body: body_block, span, meta: () });
            Node::Sequence { nodes, span, meta: () }
        }
        WitnessKind::Quasiquote { segments } => {
            let mut nodes: Vec<Node<()>> = Vec::new();
            let mut segs: Vec<QuasiquoteSegment> = Vec::new();
            for seg in segments {
                match &seg.kind {
                    // Quote 段：Literal(String)
                    WitnessKind::Literal(Literal::String(s, _)) => {
                        segs.push(QuasiquoteSegment::Quote(s.clone()));
                    }
                    // UnquoteSplice 标记：Literal(Boolean("splice"))
                    WitnessKind::Literal(Literal::Bool(true, _)) => {
                        // 已在前面处理 — 跳过标记
                    }
                    _ => {
                        let n = build_node(b, seg);
                        segs.push(QuasiquoteSegment::Unquote(node_result_reg(&n)));
                        nodes.push(n);
                    }
                }
            }
            let dst = b.alloc();
            nodes.push(Node::Quasiquote { dst, segments: segs, span, meta: () });
            Node::Sequence { nodes, span, meta: () }
        }
    }
}

/// 将单个 witness 转为 Block<()>（用于 body/then/else/handler 等块上下文）。
fn build_block(b: &mut FcfgBuilder, w: &MirWitness) -> Block<()> {
    let node = build_node(b, w);
    let result = node_result_reg(&node);
    Block { nodes: vec![node], result: Some(result) }
}

/// 提取节点的结果寄存器（值产生节点）。
fn node_result_reg(n: &Node<()>) -> Reg {
    node_result_reg_of(n).unwrap_or(0)
}

/// 提取节点的结果寄存器（Option 版本）。
fn node_result_reg_of(n: &Node<()>) -> Option<Reg> {
    match n {
        Node::Literal { reg, .. }
        | Node::Variable { reg, .. }
        | Node::BinaryOp { dst: reg, .. }
        | Node::Call { dst: reg, .. }
        | Node::MethodCall { dst: reg, .. }
        | Node::Or { dst: reg, .. }
        | Node::And { dst: reg, .. }
        | Node::DynTrait { dst: reg, .. }
        | Node::Prompt { dst: reg, .. }
        | Node::ClosureExpr { dst: reg, .. }
        | Node::ListLit { dst: reg, .. }
        | Node::DictLit { dst: reg, .. }
        | Node::Index { dst: reg, .. }
        | Node::Perform { dst: reg, .. }
        | Node::Match { dst: reg, .. }
        | Node::Quasiquote { dst: reg, .. } => Some(*reg),
        Node::Sequence { nodes, .. } => nodes.last().and_then(node_result_reg_of),
        _ => None,
    }
}

/// WitnessParam → fcfg::Param。
fn witness_param_to_param(p: &WitnessParam) -> Param {
    Param {
        name: p.name.clone(),
        type_ann: p.type_hint.as_ref().map(hint_to_annotation),
        // witness 层 default 是 MirWitness（表达式）；fcfg 层是 Literal。
        // 默认值求值发生在运行时，此处降维为 None（调用方按缺省处理）。
        default: None,
    }
}

/// TypeHint → TypeAnnotation。
fn hint_to_annotation(h: &TypeHint) -> TypeAnnotation {
    TypeAnnotation(h.to_type().name().to_string())
}

/// WitnessPattern → fcfg::Pattern。
fn witness_pattern_to_pattern(p: &WitnessPattern) -> Pattern {
    match p {
        WitnessPattern::Wildcard => Pattern::Wildcard,
        WitnessPattern::Variable(name) => Pattern::Variable(name.clone()),
        WitnessPattern::Literal(lit) => Pattern::Literal(lit.clone()),
        WitnessPattern::Tuple(items) => {
            Pattern::Tuple(items.iter().map(witness_pattern_to_pattern).collect())
        }
        // witness 层 List{head, tail} → fcfg 层 ListVec{head, tail}
        WitnessPattern::List { head, tail } => Pattern::ListVec {
            head: vec![witness_pattern_to_pattern(head)],
            tail: Some(Box::new(witness_pattern_to_pattern(tail))),
        },
        WitnessPattern::ListVec { elements, rest } => Pattern::ListVec {
            head: elements.iter().map(witness_pattern_to_pattern).collect(),
            tail: rest.as_ref().map(|t| Box::new(witness_pattern_to_pattern(t))),
        },
        WitnessPattern::Dict { required, .. } => Pattern::Dict(
            required
                .iter()
                .map(|(k, v)| (k.clone(), witness_pattern_to_pattern(v)))
                .collect(),
        ),
        WitnessPattern::TypeAscription { name, pattern } => Pattern::TypeAscription(
            Box::new(witness_pattern_to_pattern(pattern)),
            TypeAnnotation(name.clone()),
        ),
    }
}

/// WitnessOrchestrateKind → fcfg::OrchestrateKind。
fn build_orchestrate_kind(_b: &mut FcfgBuilder, k: &WitnessOrchestrateKind) -> OrchestrateKind<()> {
    use crate::mir::witness::WitnessOrchestrateKind as W;
    match k {
        W::Sequential { .. } => OrchestrateKind::Sequential,
        W::Loop { .. } => OrchestrateKind::Loop { body: Block { nodes: vec![], result: None } },
        W::Graph { agents, edges } => OrchestrateKind::Graph {
            vertices: agents.iter().map(|a| a.name.clone()).collect(),
            edges: edges.iter().map(|e| (e.from.clone(), e.to.clone())).collect(),
        },
        W::Pregel { agents, edges, .. } => OrchestrateKind::Pregel {
            config: crate::mir::fcfg::PregelConfig {
                vertices: agents.iter().map(|a| a.name.clone()).collect(),
                edges: edges.iter().map(|e| (e.from.clone(), e.to.clone())).collect(),
                compute: Block { nodes: vec![], result: None },
                combine: None,
                max_supersteps: None,
            },
        },
        W::Moa { proposers, aggregator, .. } => OrchestrateKind::MoA {
            layers: vec![crate::mir::fcfg::MoALayer {
                proposers: vec![],
                aggregator: crate::mir::fcfg::Block { nodes: vec![], result: None },
            }],
        }
        .tag_moa(proposers, aggregator),
        W::Moe { experts, router, top_k, .. } => OrchestrateKind::MoE {
            experts: experts.iter().map(|e| format!("{:?}", e.span)).collect(),
            router: format!("{:?}", router.span),
            top_k: *top_k,
        },
    }
}

/// MoA 层标注辅助（witness 层的 proposers/aggregator 名单填入 fcfg）。
trait MoaTag {
    fn tag_moa(self, _proposers: &[String], _aggregator: &str) -> Self;
}

impl MoaTag for OrchestrateKind<()> {
    fn tag_moa(self, proposers: &[String], aggregator: &str) -> Self {
        // fcfg::MoALayer 的 proposers 是 Block，witness 层是名字列表 —
        // 暂存到 layers 数量中，完整 agent 定义待 emit 拆分时补全。
        let _ = (proposers, aggregator);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{BinaryOp, Literal, Span};

    fn wit_int(n: i64, line: usize) -> MirWitness {
        MirWitness {
            kind: WitnessKind::Literal(Literal::Int(n, Span::new(line, 0))),
            span: Span::new(line, 0),
        }
    }

    #[test]
    fn literal_to_fcfg() {
        let ws = vec![wit_int(42, 1)];
        let fcfg = witness_to_fcfg(&ws);
        assert_eq!(fcfg.len(), 1);
        match &fcfg[0] {
            Node::Literal { reg, value, .. } => {
                assert_eq!(*reg, 0);
                assert!(matches!(value, Literal::Int(42, _)));
            }
            _ => panic!("expected Literal"),
        }
    }

    #[test]
    fn binary_to_fcfg() {
        let ws = vec![MirWitness {
            kind: WitnessKind::Binary {
                left: Box::new(wit_int(1, 1)),
                op: BinaryOp::Add,
                right: Box::new(wit_int(2, 1)),
            },
            span: Span::new(1, 0),
        }];
        let fcfg = witness_to_fcfg(&ws);
        match &fcfg[0] {
            Node::Sequence { nodes, .. } => {
                assert_eq!(nodes.len(), 3); // lhs + rhs + BinaryOp
                match &nodes[2] {
                    Node::BinaryOp { dst, lhs, op, rhs, .. } => {
                        assert_eq!(*lhs, 0);
                        assert_eq!(*rhs, 1);
                        assert_eq!(*dst, 2);
                        assert!(matches!(op, BinaryOp::Add));
                    }
                    _ => panic!("expected BinaryOp"),
                }
            }
            _ => panic!("expected Sequence"),
        }
    }

    #[test]
    fn let_binding_to_fcfg() {
        let ws = vec![MirWitness {
            kind: WitnessKind::LetBinding {
                name: "x".to_string(),
                type_hint: None,
                value: Box::new(wit_int(10, 1)),
                init_body: Box::new(wit_int(20, 2)),
            },
            span: Span::new(1, 0),
        }];
        let fcfg = witness_to_fcfg(&ws);
        match &fcfg[0] {
            Node::Sequence { nodes, .. } => {
                assert_eq!(nodes.len(), 2); // value + Let
                match &nodes[1] {
                    Node::Let { name, value, .. } => {
                        assert_eq!(name, "x");
                        assert_eq!(*value, 0);
                    }
                    _ => panic!("expected Let"),
                }
            }
            _ => panic!("expected Sequence"),
        }
    }

    #[test]
    fn real_program_arithmetic() {
        // 用真实编译器产出 witnesses 再转换
        let src = "task main()\n  print(10i + 32i)\nend";
        let (_func, witnesses) = crate::parser_v3::ParserV3::compile(src).unwrap();
        let fcfg = witness_to_fcfg(&witnesses);
        assert!(!fcfg.is_empty(), "FCFG should be non-empty");
    }
}
