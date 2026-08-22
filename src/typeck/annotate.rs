//! v0.89: TypeAnnotator — Phase 2 EHIR 构建。
//!
//! 将 FCFG（Node<()>）+ TypeTable（影子表）→ EHIR（Node<TypeInfo>）。
//!
//! 遍历每个 Node<()>，用其 span 查 TypeTable，将查到的 (Type, EffectRow)
//! 填入 TypeInfo meta。查不到的节点保留 TypeInfo::unknown()（降级处理，
//! 不中断标注）。

use crate::mir::fcfg::{Block, Fcfg, MatchArm, Node, TypeInfo};
use crate::typeck::export::TypeTable;

/// 将 FCFG 节点列表标注为 EHIR。
///
/// 对每个节点，用 span 查 TypeTable。查到则填充 TypeInfo，
/// 未查到则用 TypeInfo::unknown()（Any 类型 + Empty effect）。
pub fn annotate(nodes: &[Fcfg], table: &TypeTable) -> Vec<Node<TypeInfo>> {
    nodes.iter().map(|n| annotate_node(n, table)).collect()
}

/// 标注单个节点。
fn annotate_node(node: &Fcfg, table: &TypeTable) -> Node<TypeInfo> {
    let span = node.span();
    let info = table.types.get(&span).map_or_else(
        || TypeInfo::unknown(span),
        |(ty, row)| TypeInfo::new(ty.clone(), row.clone(), span),
    );

    match node {
        // ── 值产生 ──
        Node::Literal { reg, value, span, .. } => Node::Literal {
            reg: *reg,
            value: value.clone(),
            span: *span,
            meta: info,
        },
        Node::Variable { reg, name, span, .. } => Node::Variable {
            reg: *reg,
            name: name.clone(),
            span: *span,
            meta: info,
        },
        Node::BinaryOp { dst, lhs, op, rhs, span, .. } => Node::BinaryOp {
            dst: *dst,
            lhs: *lhs,
            op: op.clone(),
            rhs: *rhs,
            span: *span,
            meta: info,
        },
        Node::Call { dst, callee, callee_name, args, span, .. } => Node::Call {
            dst: *dst,
            callee: *callee,
            callee_name: callee_name.clone(),
            args: args.clone(),
            span: *span,
            meta: info,
        },
        Node::MethodCall { dst, receiver, method, args, span, .. } => Node::MethodCall {
            dst: *dst,
            receiver: *receiver,
            method: method.clone(),
            args: args.clone(),
            span: *span,
            meta: info,
        },
        Node::Or { dst, lhs, rhs, span, .. } => Node::Or {
            dst: *dst,
            lhs: *lhs,
            rhs: *rhs,
            span: *span,
            meta: info,
        },
        Node::And { dst, lhs, rhs, span, .. } => Node::And {
            dst: *dst,
            lhs: *lhs,
            rhs: *rhs,
            span: *span,
            meta: info,
        },
        Node::DynTrait { dst, src, trait_name, span, .. } => Node::DynTrait {
            dst: *dst,
            src: *src,
            trait_name: trait_name.clone(),
            span: *span,
            meta: info,
        },
        Node::Prompt { dst, parts, span, .. } => Node::Prompt {
            dst: *dst,
            parts: parts.clone(),
            span: *span,
            meta: info,
        },
        Node::ClosureExpr { dst, params, body, span, .. } => Node::ClosureExpr {
            dst: *dst,
            params: params.clone(),
            body: annotate_block(body, table),
            span: *span,
            meta: info,
        },
        Node::ListLit { dst, items, span, .. } => Node::ListLit {
            dst: *dst,
            items: items.clone(),
            span: *span,
            meta: info,
        },
        Node::DictLit { dst, entries, span, .. } => Node::DictLit {
            dst: *dst,
            entries: entries.clone(),
            span: *span,
            meta: info,
        },
        Node::Index { dst, obj, idx, span, .. } => Node::Index {
            dst: *dst,
            obj: *obj,
            idx: *idx,
            span: *span,
            meta: info,
        },

        // ── 控制流 ──
        Node::If { cond, then, else_, span, .. } => Node::If {
            cond: *cond,
            then: annotate_block(then, table),
            else_: else_.as_ref().map(|e| annotate_block(e, table)),
            span: *span,
            meta: info,
        },
        Node::While { cond, body, span, .. } => Node::While {
            cond: annotate_block(cond, table),
            body: annotate_block(body, table),
            span: *span,
            meta: info,
        },
        Node::For { var, iter, body, span, .. } => Node::For {
            var: var.clone(),
            iter: *iter,
            body: annotate_block(body, table),
            span: *span,
            meta: info,
        },
        Node::Match { scrutinee, arms, span, .. } => Node::Match {
            scrutinee: *scrutinee,
            arms: arms.iter().map(|a| annotate_arm(a, table)).collect(),
            span: *span,
            meta: info,
        },
        Node::Return { value, span, .. } => Node::Return {
            value: *value,
            span: *span,
            meta: info,
        },
        Node::Break { span, .. } => Node::Break {
            span: *span,
            meta: info,
        },
        Node::Continue { span, .. } => Node::Continue {
            span: *span,
            meta: info,
        },

        // ── 绑定 ──
        Node::Let { name, type_ann, value, body, span, .. } => Node::Let {
            name: name.clone(),
            type_ann: type_ann.clone(),
            value: *value,
            body: annotate_block(body, table),
            span: *span,
            meta: info,
        },
        Node::Assign { name, value, span, .. } => Node::Assign {
            name: name.clone(),
            value: *value,
            span: *span,
            meta: info,
        },
        Node::IndexAssign { obj, idx, value, span, .. } => Node::IndexAssign {
            obj: *obj,
            idx: *idx,
            value: *value,
            span: *span,
            meta: info,
        },

        // ── 声明 ──
        Node::FnDef { name, params, return_ann, body, span, .. } => Node::FnDef {
            name: name.clone(),
            params: params.clone(),
            return_ann: return_ann.clone(),
            body: annotate_block(body, table),
            span: *span,
            meta: info,
        },
        Node::TypeAlias { name, target, span, .. } => Node::TypeAlias {
            name: name.clone(),
            target: target.clone(),
            span: *span,
            meta: info,
        },
        Node::EnumDef { name, variants, span, .. } => Node::EnumDef {
            name: name.clone(),
            variants: variants.clone(),
            span: *span,
            meta: info,
        },
        Node::StructDef { name, fields, span, .. } => Node::StructDef {
            name: name.clone(),
            fields: fields.clone(),
            span: *span,
            meta: info,
        },
        Node::TraitDef { name, methods, span, .. } => Node::TraitDef {
            name: name.clone(),
            methods: methods.clone(),
            span: *span,
            meta: info,
        },
        Node::ImplDef { trait_name, for_type, methods, span, .. } => Node::ImplDef {
            trait_name: trait_name.clone(),
            for_type: for_type.clone(),
            methods: methods.iter().map(|(n, b)| (n.clone(), annotate_block(b, table))).collect(),
            span: *span,
            meta: info,
        },
        Node::Import { path, span, .. } => Node::Import {
            path: path.clone(),
            span: *span,
            meta: info,
        },
        Node::MacroDef { name, params, body, span, .. } => Node::MacroDef {
            name: name.clone(),
            params: params.clone(),
            body: annotate_block(body, table),
            span: *span,
            meta: info,
        },

        // ── 效果 ──
        Node::Perform { effect, args, span, .. } => Node::Perform {
            effect: effect.clone(),
            args: args.clone(),
            span: *span,
            meta: info,
        },
        Node::Handle { effect, body, handler, k_param, span, .. } => Node::Handle {
            effect: effect.clone(),
            body: annotate_block(body, table),
            handler: annotate_block(handler, table),
            k_param: k_param.clone(),
            span: *span,
            meta: info,
        },

        // ── TEA ──
        Node::ModelDef { name, fields, span, .. } => Node::ModelDef {
            name: name.clone(),
            fields: fields.clone(),
            span: *span,
            meta: info,
        },
        Node::MsgDef { name, variants, span, .. } => Node::MsgDef {
            name: name.clone(),
            variants: variants.clone(),
            span: *span,
            meta: info,
        },
        Node::UpdateDef { name, params, body, span, .. } => Node::UpdateDef {
            name: name.clone(),
            params: params.clone(),
            body: annotate_block(body, table),
            span: *span,
            meta: info,
        },
        Node::AppDef { name, model, msg, init, update, view, span, .. } => Node::AppDef {
            name: name.clone(),
            model: model.clone(),
            msg: msg.clone(),
            init: annotate_block(init, table),
            update: annotate_block(update, table),
            view: annotate_block(view, table),
            span: *span,
            meta: info,
        },

        // ── 其它 ──
        Node::Quasiquote { dst, segments, span, .. } => Node::Quasiquote {
            dst: *dst,
            segments: segments.clone(),
            span: *span,
            meta: info,
        },
        Node::Orchestrate { input_var, result_var, kind, span, .. } => Node::Orchestrate {
            input_var: input_var.clone(),
            result_var: result_var.clone(),
            kind: annotate_orchestrate_kind(kind, table),
            span: *span,
            meta: info,
        },
        Node::WithConfig { bindings, body, span, .. } => Node::WithConfig {
            bindings: bindings.clone(),
            body: annotate_block(body, table),
            span: *span,
            meta: info,
        },
        Node::Sequence { nodes, span, .. } => Node::Sequence {
            nodes: annotate(nodes, table),
            span: *span,
            meta: info,
        },
        Node::Expr { reg, span, .. } => Node::Expr {
            reg: *reg,
            span: *span,
            meta: info,
        },
    }
}

fn annotate_block(block: &Block<()>, table: &TypeTable) -> Block<TypeInfo> {
    Block {
        nodes: annotate(&block.nodes, table),
        result: block.result,
    }
}

fn annotate_arm(arm: &MatchArm<()>, table: &TypeTable) -> MatchArm<TypeInfo> {
    MatchArm {
        pattern: arm.pattern.clone(),
        guard: arm.guard,
        body: annotate_block(&arm.body, table),
    }
}

fn annotate_orchestrate_kind(
    kind: &crate::mir::fcfg::OrchestrateKind<()>,
    table: &TypeTable,
) -> crate::mir::fcfg::OrchestrateKind<TypeInfo> {
    use crate::mir::fcfg::OrchestrateKind;
    match kind {
        OrchestrateKind::Sequential => OrchestrateKind::Sequential,
        OrchestrateKind::Loop { body } => OrchestrateKind::Loop {
            body: annotate_block(body, table),
        },
        OrchestrateKind::Graph { vertices, edges } => OrchestrateKind::Graph {
            vertices: vertices.clone(),
            edges: edges.clone(),
        },
        OrchestrateKind::Pregel { config } => OrchestrateKind::Pregel {
            config: crate::mir::fcfg::PregelConfig {
                vertices: config.vertices.clone(),
                edges: config.edges.clone(),
                compute: annotate_block(&config.compute, table),
                combine: config.combine.as_ref().map(|c| annotate_block(c, table)),
                max_supersteps: config.max_supersteps,
            },
        },
        OrchestrateKind::MoA { layers } => OrchestrateKind::MoA {
            layers: layers
                .iter()
                .map(|l| crate::mir::fcfg::MoALayer {
                    proposers: l.proposers.iter().map(|p| annotate_block(p, table)).collect(),
                    aggregator: annotate_block(&l.aggregator, table),
                })
                .collect(),
        },
        OrchestrateKind::MoE { experts, router, top_k } => OrchestrateKind::MoE {
            experts: experts.clone(),
            router: router.clone(),
            top_k: *top_k,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{Literal, Span};
    use crate::mir::effect::EffectRow;
    use crate::typeck::Type;

    const S: Span = Span { line: 42, column: 0 };

    #[test]
    fn annotate_literal_with_type() {
        let fcfg = vec![Node::Literal {
            reg: 0,
            value: Literal::Int(42, S),
            span: S,
            meta: (),
        }];
        let mut table = TypeTable { types: std::collections::HashMap::new() };
        table.types.insert(S, (Type::Int, EffectRow::Empty));

        let ehir = annotate(&fcfg, &table);
        assert_eq!(ehir.len(), 1);
        match &ehir[0] {
            Node::Literal { meta, .. } => {
                assert_eq!(meta.ty, Type::Int);
                assert!(meta.effects.is_empty());
            }
            _ => panic!("expected Literal"),
        }
    }

    #[test]
    fn annotate_missing_span_fallback() {
        let fcfg = vec![Node::Literal {
            reg: 0,
            value: Literal::Int(1, S),
            span: S,
            meta: (),
        }];
        let table = TypeTable { types: std::collections::HashMap::new() };

        let ehir = annotate(&fcfg, &table);
        match &ehir[0] {
            Node::Literal { meta, .. } => {
                assert_eq!(meta.ty, Type::Any, "missing span should fall back to Any");
            }
            _ => panic!("expected Literal"),
        }
    }
}
