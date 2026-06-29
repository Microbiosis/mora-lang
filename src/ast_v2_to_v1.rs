//! v0.24: 反向适配层
//!
//! 将新 AST (ast_v2.rs) 转换为旧 AST (ast.rs)
//! 支持渐进式迁移：主程序可使用 ParserV2，解释器继续使用旧 AST

use crate::ast::{self, Expr, FnDef, Literal, ObserveConfig, Stmt, TraitMethod};
use crate::ast_v2::{AstArena, ExprKind, NodeId, StmtKind};

/// 反向适配器：将 ast_v2 转换为 ast
pub struct AstV2ToV1 {
    arena: AstArena,
}

impl AstV2ToV1 {
    pub fn new(arena: AstArena) -> Self {
        Self { arena }
    }

    /// 转换整个程序
    pub fn convert_program(&self, stmts: &[NodeId]) -> Vec<Stmt> {
        stmts.iter().map(|s| self.convert_stmt(*s)).collect()
    }

    /// 转换语句
    fn convert_stmt(&self, id: NodeId) -> Stmt {
        let stmt = self.arena.stmts.get(id.0).unwrap_or_else(|| {
            panic!("Invalid statement NodeId({}), stmts.len={}", id.0, self.arena.stmts.len())
        });
        let span = stmt.span;
        match &stmt.kind {
            StmtKind::Let { name, type_hint, init, exported } => Stmt::Let {
                name: name.clone(),
                type_hint: type_hint.clone(),
                init: self.convert_expr(*init),
                exported: *exported,
                span,
            },
            StmtKind::Assign { name, value } => Stmt::Assign {
                name: name.clone(),
                value: self.convert_expr(*value),
                span,
            },
            StmtKind::IndexAssign { object, index, value } => Stmt::IndexAssign {
                object: self.convert_expr(*object),
                index: self.convert_expr(*index),
                value: self.convert_expr(*value),
                span,
            },
            StmtKind::TaskDef { name, lifetime_params, params, return_type, body, exported } => Stmt::TaskDef {
                name: name.clone(),
                lifetime_params: lifetime_params.clone(),
                params: params.clone(),
                return_type: return_type.clone(),
                body: self.convert_stmts(body),
                exported: *exported,
                span,
            },
            StmtKind::If { condition, then_branch, else_branch: _ } => {
                // 旧 AST 没有 else_branch，只处理 then_branch
                Stmt::If {
                    condition: self.convert_expr(*condition),
                    then_branch: self.convert_stmts(then_branch),
                    span,
                }
            },
            StmtKind::For { var, var_type, iterable, body } => Stmt::For {
                var: var.clone(),
                var_type: var_type.clone(),
                iterable: self.convert_expr(*iterable),
                body: self.convert_stmts(body),
                span,
            },
            StmtKind::Return { value } => Stmt::Return {
                value: value.map(|v| self.convert_expr(v)),
                span,
            },
            StmtKind::Import { path } => Stmt::Import {
                path: path.clone(),
                span,
            },
            StmtKind::Parallel { stmts } => Stmt::Parallel {
                stmts: self.convert_stmts(stmts),
                span,
            },
            StmtKind::Match { expr, arms } => Stmt::Match {
                expr: self.convert_expr(*expr),
                arms: arms.iter().map(|(p, body)| {
                    (self.convert_pattern(p), self.convert_stmts(body))
                }).collect(),
                span,
            },
            StmtKind::Save { path, value } => Stmt::Save {
                path: self.convert_expr(*path),
                value: self.convert_expr(*value),
                span,
            },
            StmtKind::Load { path, var } => Stmt::Load {
                path: self.convert_expr(*path),
                var: var.clone(),
                span,
            },
            StmtKind::ReadFile { path, var } => Stmt::ReadFile {
                path: self.convert_expr(*path),
                var: var.clone(),
                span,
            },
            StmtKind::WriteFile { path, content } => Stmt::WriteFile {
                path: self.convert_expr(*path),
                content: self.convert_expr(*content),
                span,
            },
            StmtKind::AppendFile { path, content } => Stmt::AppendFile {
                path: self.convert_expr(*path),
                content: self.convert_expr(*content),
                span,
            },
            StmtKind::ReadBytesFile { path, var } => Stmt::ReadBytesFile {
                path: self.convert_expr(*path),
                var: var.clone(),
                span,
            },
            StmtKind::WriteBytesFile { path, content } => Stmt::WriteBytesFile {
                path: self.convert_expr(*path),
                content: self.convert_expr(*content),
                span,
            },
            StmtKind::Expr(id) => Stmt::Expr(self.convert_expr(*id)),
            StmtKind::With { bindings, body } => Stmt::With {
                bindings: bindings.iter().map(|(k, v)| (k.clone(), self.convert_expr(*v))).collect(),
                body: self.convert_stmts(body),
                span,
            },
            StmtKind::StreamFor { prompt, var, body } => Stmt::StreamFor {
                prompt: self.convert_expr(*prompt),
                var: var.clone(),
                body: self.convert_stmts(body),
                span,
            },
            StmtKind::ToolDef { name, params, return_type, body, exported } => Stmt::ToolDef {
                name: name.clone(),
                params: params.clone(),
                return_type: return_type.clone(),
                body: self.convert_stmts(body),
                exported: *exported,
                span,
            },
            StmtKind::Break => Stmt::Break { span },
            StmtKind::Continue => Stmt::Continue { span },
            StmtKind::Route { name, target } => Stmt::Route {
                name: name.clone(),
                target: self.convert_expr(*target),
                span,
            },
            StmtKind::Observe { config, body } => Stmt::Observe {
                config: self.convert_observe_config(config),
                body: self.convert_stmts(body),
                span,
            },
            StmtKind::Span { name, attributes, body } => Stmt::Span {
                name: name.clone(),
                attributes: attributes.iter().map(|(k, v)| (k.clone(), self.convert_expr(*v))).collect(),
                body: self.convert_stmts(body),
                span,
            },
            StmtKind::RecordTokens { input, output } => Stmt::RecordTokens {
                input: self.convert_expr(*input),
                output: self.convert_expr(*output),
                span,
            },
            StmtKind::TraitDef { name, generics, parents, trait_where, methods } => Stmt::TraitDef {
                name: name.clone(),
                generics: generics.clone(),
                parents: parents.clone(),
                trait_where: trait_where.clone(),
                methods: methods.iter().map(|m| self.convert_trait_method(m)).collect(),
                span,
            },
            StmtKind::ImplDef { generics, trait_generics, trait_name, for_type, for_generics, where_clause, methods } => Stmt::ImplDef {
                generics: generics.clone(),
                trait_generics: trait_generics.clone(),
                trait_name: trait_name.clone(),
                for_type: for_type.clone(),
                for_generics: for_generics.clone(),
                where_clause: where_clause.clone(),
                methods: methods.iter().map(|m| self.convert_fn_def(m)).collect(),
                span,
            },
            StmtKind::Worker { name, body } => Stmt::Worker {
                name: name.clone(),
                body: self.convert_stmts(body),
                span,
            },
            StmtKind::Send { value, target } => Stmt::Send {
                value: self.convert_expr(*value),
                target: target.clone(),
                span,
            },
            StmtKind::Receive { var, source } => Stmt::Receive {
                var: var.clone(),
                source: source.clone(),
                span,
            },
            StmtKind::Transaction { body, compensation } => Stmt::Transaction {
                body: self.convert_stmts(body),
                compensation: self.convert_stmts(compensation),
                span,
            },
            StmtKind::Commit => Stmt::Commit { span },
            StmtKind::Rollback => Stmt::Rollback { span },
            StmtKind::MacroDef { name, params, body } => Stmt::MacroDef {
                name: name.clone(),
                params: params.clone(),
                body: self.convert_stmts(body),
                span,
            },
            StmtKind::TypeAlias { name, generics, target } => Stmt::TypeAlias {
                name: name.clone(),
                generics: generics.clone(),
                target: target.clone(),
                span,
            },
            StmtKind::EnumDef { name, generics, variants } => Stmt::EnumDef {
                name: name.clone(),
                generics: generics.clone(),
                variants: variants.clone(),
                span,
            },
            StmtKind::StructDef { name, generics, fields } => Stmt::StructDef {
                name: name.clone(),
                generics: generics.clone(),
                fields: fields.clone(),
                span,
            },
        }
    }

    /// 转换表达式
    fn convert_expr(&self, id: NodeId) -> Expr {
        let expr = self.arena.exprs.get(id.0).expect("Invalid expression NodeId");
        let span = expr.span;
        match &expr.kind {
            ExprKind::Literal(lit) => Expr::Literal(lit.clone()),
            ExprKind::Variable(name) => Expr::Variable(name.clone(), span),
            ExprKind::Binary { left, op, right } => Expr::Binary {
                left: Box::new(self.convert_expr(*left)),
                op: op.clone(),
                right: Box::new(self.convert_expr(*right)),
                span,
            },
            ExprKind::Pipe { left, right } => Expr::Pipe {
                left: Box::new(self.convert_expr(*left)),
                right: Box::new(self.convert_expr(*right)),
                span,
            },
            ExprKind::Call { callee, args } => Expr::Call {
                callee: callee.clone(),
                args: args.iter().map(|a| Box::new(self.convert_expr(*a))).collect(),
                span,
            },
            ExprKind::MethodCall { object, method, args } => Expr::MethodCall {
                object: Box::new(self.convert_expr(*object)),
                method: method.clone(),
                args: args.iter().map(|a| Box::new(self.convert_expr(*a))).collect(),
                span,
            },
            ExprKind::Index { object, index } => Expr::Index {
                object: Box::new(self.convert_expr(*object)),
                index: Box::new(self.convert_expr(*index)),
                span,
            },
            ExprKind::Question { expr } => Expr::Question {
                expr: Box::new(self.convert_expr(*expr)),
                span,
            },
            ExprKind::Closure { params, return_type, body } => Expr::Closure {
                params: params.clone(),
                return_type: return_type.clone(),
                body: self.convert_stmts(body),
                span,
            },
            ExprKind::Match { expr, arms } => Expr::Match {
                expr: Box::new(self.convert_expr(*expr)),
                arms: arms.iter().map(|(p, e)| {
                    (self.convert_pattern(p), Box::new(self.convert_expr(*e)))
                }).collect(),
                span,
            },
            ExprKind::Prompt { parts } => Expr::Prompt {
                parts: parts.iter().map(|p| self.convert_expr(*p)).collect(),
                span,
            },
            ExprKind::RouteCall { name, args } => Expr::RouteCall {
                name: name.clone(),
                args: args.iter().map(|a| Box::new(self.convert_expr(*a))).collect(),
                span,
            },
            ExprKind::AiModelCall { model, temperature, max_tokens, system } => Expr::AiModelCall {
                model: Box::new(self.convert_expr(*model)),
                temperature: temperature.map(|t| Box::new(self.convert_expr(t))),
                max_tokens: max_tokens.map(|t| Box::new(self.convert_expr(t))),
                system: system.map(|s| Box::new(self.convert_expr(s))),
                span,
            },
            ExprKind::NamespaceRef { namespace, name } => Expr::NamespaceRef {
                namespace: namespace.clone(),
                name: name.clone(),
                span,
            },
            ExprKind::Grouping(id) => Expr::Grouping(Box::new(self.convert_expr(*id)), span),
            ExprKind::List(items) => Expr::Literal(Literal::List(
                items.iter().map(|id| Box::new(self.convert_expr(*id))).collect(),
                span,
            )),
            ExprKind::Dict(entries) => Expr::Literal(Literal::Dict(
                entries.iter().map(|(k, id)| (k.clone(), Box::new(self.convert_expr(*id)))).collect(),
                span,
            )),
            ExprKind::DynTrait { generics: _, trait_name } => Expr::Variable(format!("dyn:{}", trait_name), span),
            ExprKind::Borrow { expr } => Expr::Borrow {
                expr: Box::new(self.convert_expr(*expr)),
                span,
            },
            ExprKind::BorrowMut { expr } => Expr::BorrowMut {
                expr: Box::new(self.convert_expr(*expr)),
                span,
            },
        }
    }

    /// 转换模式
    fn convert_pattern(&self, pattern: &crate::ast_v2::Pattern) -> ast::Pattern {
        match pattern {
            crate::ast_v2::Pattern::Wildcard => ast::Pattern::Wildcard,
            crate::ast_v2::Pattern::Literal(lit) => ast::Pattern::Literal(lit.clone()),
            crate::ast_v2::Pattern::Variable(name) => ast::Pattern::Variable(name.clone()),
            crate::ast_v2::Pattern::List { prefix, rest } => ast::Pattern::List {
                prefix: prefix.iter().map(|p| self.convert_pattern(p)).collect(),
                rest: rest.clone(),
            },
            crate::ast_v2::Pattern::Dict(entries) => ast::Pattern::Dict(
                entries.iter().map(|(k, p)| (k.clone(), self.convert_pattern(p))).collect(),
            ),
            crate::ast_v2::Pattern::Guard { pattern, condition } => ast::Pattern::Guard {
                pattern: Box::new(self.convert_pattern(pattern)),
                condition: Box::new(self.convert_expr(*condition)),
            },
        }
    }

    /// 转换 ObserveConfig
    fn convert_observe_config(&self, config: &crate::ast_v2::ObserveConfig) -> ObserveConfig {
        match config {
            crate::ast_v2::ObserveConfig::Trace => ObserveConfig::Trace,
            crate::ast_v2::ObserveConfig::Metrics => ObserveConfig::Metrics,
            crate::ast_v2::ObserveConfig::Otel { endpoint } => ObserveConfig::Otel {
                endpoint: self.convert_expr(*endpoint),
            },
        }
    }

    /// 转换 FnDef
    fn convert_fn_def(&self, fndef: &crate::ast_v2::FnDef) -> FnDef {
        FnDef {
            name: fndef.name.clone(),
            params: fndef.params.clone(),
            return_type: fndef.return_type.clone(),
            body: self.convert_stmts(&fndef.body),
            span: fndef.span,
        }
    }

    /// 转换 TraitMethod
    fn convert_trait_method(&self, method: &crate::ast_v2::TraitMethod) -> TraitMethod {
        TraitMethod {
            name: method.name.clone(),
            params: method.params.clone(),
            return_type: method.return_type.clone(),
            body: self.convert_stmts(&method.body),
            generics: method.generics.clone(),
            span: method.span,
        }
    }

    /// 转换语句列表
    fn convert_stmts(&self, stmts: &[NodeId]) -> Vec<Stmt> {
        stmts.iter().map(|s| self.convert_stmt(*s)).collect()
    }
}
