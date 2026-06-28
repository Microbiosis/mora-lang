//! v0.22: Typed AST 转换器
//!
//! 将旧 AST (ast.rs) 转换为新 Typed AST (ast_v2.rs)
//! 保留类型信息，支持 Arena 分配

use crate::ast::{Expr, Literal, Span, Stmt};
use crate::ast_v2::{AstArena, ExprKind, NodeId, StmtKind};

/// Typed AST 转换器
pub struct TypedAstConverter {
    arena: AstArena,
}

impl Default for TypedAstConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl TypedAstConverter {
    pub fn new() -> Self {
        Self {
            arena: AstArena::new(),
        }
    }

    /// 转换整个程序
    pub fn convert_program(&mut self, stmts: &[Stmt]) -> Vec<NodeId> {
        stmts.iter().map(|s| self.convert_stmt(s)).collect()
    }

    /// 转换语句
    pub fn convert_stmt(&mut self, stmt: &Stmt) -> NodeId {
        let span = self.get_stmt_span(stmt);
        let kind = match stmt {
            Stmt::Let {
                name,
                type_hint,
                init,
                exported,
                ..
            } => StmtKind::Let {
                name: name.clone(),
                type_hint: type_hint.clone(),
                init: self.convert_expr(init),
                exported: *exported,
            },

            Stmt::Assign { name, value, .. } => StmtKind::Assign {
                name: name.clone(),
                value: self.convert_expr(value),
            },

            Stmt::IndexAssign {
                object,
                index,
                value,
                ..
            } => StmtKind::IndexAssign {
                object: self.convert_expr(object),
                index: self.convert_expr(index),
                value: self.convert_expr(value),
            },

            Stmt::TaskDef {
                name,
                lifetime_params,
                params,
                return_type,
                body,
                exported,
                ..
            } => StmtKind::TaskDef {
                name: name.clone(),
                lifetime_params: lifetime_params.clone(),
                params: params.clone(),
                return_type: return_type.clone(),
                body: self.convert_stmts(body),
                exported: *exported,
            },

            Stmt::If {
                condition,
                then_branch,
                ..
            } => StmtKind::If {
                condition: self.convert_expr(condition),
                then_branch: self.convert_stmts(then_branch),
                else_branch: vec![],
            },

            Stmt::For {
                var,
                var_type,
                iterable,
                body,
                ..
            } => StmtKind::For {
                var: var.clone(),
                var_type: var_type.clone(),
                iterable: self.convert_expr(iterable),
                body: self.convert_stmts(body),
            },

            Stmt::Return { value, .. } => StmtKind::Return {
                value: value.as_ref().map(|v| self.convert_expr(v)),
            },

            Stmt::Import { path, .. } => StmtKind::Import { path: path.clone() },

            Stmt::Parallel { stmts, .. } => StmtKind::Parallel {
                stmts: self.convert_stmts(stmts),
            },

            Stmt::Match { expr, arms, .. } => StmtKind::Match {
                expr: self.convert_expr(expr),
                arms: arms
                    .iter()
                    .map(|(p, s)| (p.clone(), self.convert_stmts(s)))
                    .collect(),
            },

            Stmt::Save { path, value, .. } => StmtKind::Save {
                path: self.convert_expr(path),
                value: self.convert_expr(value),
            },

            Stmt::Load { path, var, .. } => StmtKind::Load {
                path: self.convert_expr(path),
                var: var.clone(),
            },

            Stmt::ReadFile { path, var, .. } => StmtKind::ReadFile {
                path: self.convert_expr(path),
                var: var.clone(),
            },

            Stmt::WriteFile { path, content, .. } => StmtKind::WriteFile {
                path: self.convert_expr(path),
                content: self.convert_expr(content),
            },

            Stmt::AppendFile { path, content, .. } => StmtKind::AppendFile {
                path: self.convert_expr(path),
                content: self.convert_expr(content),
            },

            Stmt::ReadBytesFile { path, var, .. } => StmtKind::ReadBytesFile {
                path: self.convert_expr(path),
                var: var.clone(),
            },

            Stmt::WriteBytesFile { path, content, .. } => StmtKind::WriteBytesFile {
                path: self.convert_expr(path),
                content: self.convert_expr(content),
            },

            Stmt::Expr(expr) => StmtKind::Expr(self.convert_expr(expr)),

            Stmt::With { bindings, body, .. } => StmtKind::With {
                bindings: bindings
                    .iter()
                    .map(|(k, v)| (k.clone(), self.convert_expr(v)))
                    .collect(),
                body: self.convert_stmts(body),
            },

            Stmt::StreamFor {
                prompt, var, body, ..
            } => StmtKind::StreamFor {
                prompt: self.convert_expr(prompt),
                var: var.clone(),
                body: self.convert_stmts(body),
            },

            Stmt::ToolDef {
                name,
                params,
                return_type,
                body,
                exported,
                ..
            } => StmtKind::ToolDef {
                name: name.clone(),
                params: params.clone(),
                return_type: return_type.clone(),
                body: self.convert_stmts(body),
                exported: *exported,
            },

            Stmt::Break { .. } => StmtKind::Break,
            Stmt::Continue { .. } => StmtKind::Continue,

            Stmt::Route { name, target, .. } => StmtKind::Route {
                name: name.clone(),
                target: self.convert_expr(target),
            },

            Stmt::Observe { config, body, .. } => StmtKind::Observe {
                config: config.clone(),
                body: self.convert_stmts(body),
            },

            Stmt::Span {
                name,
                attributes,
                body,
                ..
            } => StmtKind::Span {
                name: name.clone(),
                attributes: attributes
                    .iter()
                    .map(|(k, v)| (k.clone(), self.convert_expr(v)))
                    .collect(),
                body: self.convert_stmts(body),
            },

            Stmt::RecordTokens { input, output, .. } => StmtKind::RecordTokens {
                input: self.convert_expr(input),
                output: self.convert_expr(output),
            },

            Stmt::TraitDef {
                name,
                generics,
                parents,
                trait_where,
                methods,
                ..
            } => StmtKind::TraitDef {
                name: name.clone(),
                generics: generics.clone(),
                parents: parents.clone(),
                trait_where: trait_where.clone(),
                methods: methods.clone(),
            },

            Stmt::ImplDef {
                generics,
                trait_generics,
                trait_name,
                for_type,
                for_generics,
                where_clause,
                methods,
                ..
            } => StmtKind::ImplDef {
                generics: generics.clone(),
                trait_generics: trait_generics.clone(),
                trait_name: trait_name.clone(),
                for_type: for_type.clone(),
                for_generics: for_generics.clone(),
                where_clause: where_clause.clone(),
                methods: methods.clone(),
            },

            Stmt::Worker { name, body, .. } => StmtKind::Worker {
                name: name.clone(),
                body: self.convert_stmts(body),
            },

            Stmt::Send { value, target, .. } => StmtKind::Send {
                value: self.convert_expr(value),
                target: target.clone(),
            },

            Stmt::Receive { var, source, .. } => StmtKind::Receive {
                var: var.clone(),
                source: source.clone(),
            },

            Stmt::Transaction {
                body, compensation, ..
            } => StmtKind::Transaction {
                body: self.convert_stmts(body),
                compensation: self.convert_stmts(compensation),
            },

            Stmt::Commit { .. } => StmtKind::Commit,
            Stmt::Rollback { .. } => StmtKind::Rollback,

            Stmt::MacroDef {
                name, params, body, ..
            } => StmtKind::MacroDef {
                name: name.clone(),
                params: params.clone(),
                body: self.convert_stmts(body),
            },

            // v0.23: 类型系统增强
            Stmt::TypeAlias {
                name,
                generics,
                target,
                ..
            } => StmtKind::TypeAlias {
                name: name.clone(),
                generics: generics.clone(),
                target: target.clone(),
            },
            Stmt::EnumDef {
                name,
                generics,
                variants,
                ..
            } => StmtKind::EnumDef {
                name: name.clone(),
                generics: generics.clone(),
                variants: variants.clone(),
            },
            Stmt::StructDef {
                name,
                generics,
                fields,
                ..
            } => StmtKind::StructDef {
                name: name.clone(),
                generics: generics.clone(),
                fields: fields.clone(),
            },
        };

        self.arena.alloc_stmt(kind, span)
    }

    /// 转换表达式
    pub fn convert_expr(&mut self, expr: &Expr) -> NodeId {
        let span = self.get_expr_span(expr);
        let kind = match expr {
            Expr::Literal(lit) => ExprKind::Literal(lit.clone()),

            Expr::Variable(name, _) => ExprKind::Variable(name.clone()),

            Expr::Binary {
                left, op, right, ..
            } => ExprKind::Binary {
                left: self.convert_expr(left),
                op: op.clone(),
                right: self.convert_expr(right),
            },

            Expr::Pipe { left, right, .. } => ExprKind::Pipe {
                left: self.convert_expr(left),
                right: self.convert_expr(right),
            },

            Expr::Call { callee, args, .. } => ExprKind::Call {
                callee: callee.clone(),
                args: args.iter().map(|a| self.convert_expr(a)).collect(),
            },

            Expr::MethodCall {
                object,
                method,
                args,
                ..
            } => ExprKind::MethodCall {
                object: self.convert_expr(object),
                method: method.clone(),
                args: args.iter().map(|a| self.convert_expr(a)).collect(),
            },

            Expr::Index { object, index, .. } => ExprKind::Index {
                object: self.convert_expr(object),
                index: self.convert_expr(index),
            },

            Expr::Closure {
                params,
                return_type,
                body,
                ..
            } => ExprKind::Closure {
                params: params.clone(),
                return_type: return_type.clone(),
                body: self.convert_stmts(body),
            },

            Expr::Match { expr, arms, .. } => ExprKind::Match {
                expr: self.convert_expr(expr),
                arms: arms
                    .iter()
                    .map(|(p, e)| (p.clone(), self.convert_expr(e)))
                    .collect(),
            },

            Expr::Prompt { parts, .. } => ExprKind::Prompt {
                parts: parts.iter().map(|p| self.convert_expr(p)).collect(),
            },

            Expr::RouteCall { name, args, .. } => ExprKind::RouteCall {
                name: name.clone(),
                args: args.iter().map(|a| self.convert_expr(a)).collect(),
            },

            Expr::AiModelCall {
                model,
                temperature,
                max_tokens,
                system,
                ..
            } => ExprKind::AiModelCall {
                model: self.convert_expr(model),
                temperature: temperature.as_ref().map(|t| self.convert_expr(t)),
                max_tokens: max_tokens.as_ref().map(|m| self.convert_expr(m)),
                system: system.as_ref().map(|s| self.convert_expr(s)),
            },

            Expr::Question { expr, .. } => ExprKind::Question {
                expr: self.convert_expr(expr),
            },

            Expr::NamespaceRef {
                namespace, name, ..
            } => ExprKind::NamespaceRef {
                namespace: namespace.clone(),
                name: name.clone(),
            },

            Expr::DynTrait {
                generics,
                trait_name,
                ..
            } => ExprKind::DynTrait {
                generics: generics.clone(),
                trait_name: trait_name.clone(),
            },

            Expr::Grouping(inner, _) => ExprKind::Grouping(self.convert_expr(inner)),

            Expr::Borrow { expr, .. } => ExprKind::Borrow {
                expr: self.convert_expr(expr),
            },

            Expr::BorrowMut { expr, .. } => ExprKind::BorrowMut {
                expr: self.convert_expr(expr),
            },
        };

        self.arena.alloc_expr(kind, span)
    }

    /// 转换语句列表
    fn convert_stmts(&mut self, stmts: &[Stmt]) -> Vec<NodeId> {
        stmts.iter().map(|s| self.convert_stmt(s)).collect()
    }

    /// 获取语句的 Span
    fn get_stmt_span(&self, stmt: &Stmt) -> Span {
        match stmt {
            Stmt::Let { span, .. }
            | Stmt::Assign { span, .. }
            | Stmt::IndexAssign { span, .. }
            | Stmt::TaskDef { span, .. }
            | Stmt::If { span, .. }
            | Stmt::For { span, .. }
            | Stmt::Return { span, .. }
            | Stmt::Import { span, .. }
            | Stmt::Parallel { span, .. }
            | Stmt::Match { span, .. }
            | Stmt::Save { span, .. }
            | Stmt::Load { span, .. }
            | Stmt::ReadFile { span, .. }
            | Stmt::WriteFile { span, .. }
            | Stmt::AppendFile { span, .. }
            | Stmt::ReadBytesFile { span, .. }
            | Stmt::WriteBytesFile { span, .. }
            | Stmt::With { span, .. }
            | Stmt::StreamFor { span, .. }
            | Stmt::ToolDef { span, .. }
            | Stmt::Break { span }
            | Stmt::Continue { span }
            | Stmt::Route { span, .. }
            | Stmt::Observe { span, .. }
            | Stmt::Span { span, .. }
            | Stmt::RecordTokens { span, .. }
            | Stmt::TraitDef { span, .. }
            | Stmt::ImplDef { span, .. }
            | Stmt::Worker { span, .. }
            | Stmt::Send { span, .. }
            | Stmt::Receive { span, .. }
            | Stmt::Transaction { span, .. }
            | Stmt::Commit { span }
            | Stmt::Rollback { span }
            | Stmt::MacroDef { span, .. } => *span,
            // v0.23: 类型系统增强
            Stmt::TypeAlias { span, .. }
            | Stmt::EnumDef { span, .. }
            | Stmt::StructDef { span, .. } => *span,
            Stmt::Expr(expr) => self.get_expr_span(expr),
        }
    }

    /// 获取表达式的 Span
    fn get_expr_span(&self, expr: &Expr) -> Span {
        match expr {
            Expr::Binary { span, .. }
            | Expr::Pipe { span, .. }
            | Expr::Call { span, .. }
            | Expr::MethodCall { span, .. }
            | Expr::Index { span, .. }
            | Expr::Closure { span, .. }
            | Expr::Match { span, .. }
            | Expr::Prompt { span, .. }
            | Expr::RouteCall { span, .. }
            | Expr::AiModelCall { span, .. }
            | Expr::Question { span, .. }
            | Expr::NamespaceRef { span, .. }
            | Expr::DynTrait { span, .. }
            | Expr::Borrow { span, .. }
            | Expr::BorrowMut { span, .. } => *span,
            Expr::Literal(lit) => match lit {
                Literal::String(_, s)
                | Literal::Number(_, s)
                | Literal::Bool(_, s)
                | Literal::Char(_, s)
                | Literal::Nil(s)
                | Literal::List(_, s)
                | Literal::Dict(_, s) => *s,
            },
            Expr::Variable(_, span) | Expr::Grouping(_, span) => *span,
        }
    }

    /// 获取 Arena
    pub fn arena(&self) -> &AstArena {
        &self.arena
    }

    /// 获取可变 Arena
    pub fn arena_mut(&mut self) -> &mut AstArena {
        &mut self.arena
    }

    /// 转换完成，返回 Arena
    pub fn into_arena(self) -> AstArena {
        self.arena
    }
}
