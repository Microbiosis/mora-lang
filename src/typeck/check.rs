//! 类型检查核心模块
//!
//! 从 typeck.rs 提取的 check_stmt 和 check_expr 函数

use super::*;
use crate::ast_v2::{AstArena, ExprKind, NodeId, StmtKind};
use crate::common::{BinaryOp, Span};

impl TypeChecker {
    /// 检查语句
    pub fn check_stmt(&mut self, kind: &StmtKind, arena: &AstArena, symbols: &mut SymbolTable) {
        match kind {
            StmtKind::Let {
                name,
                type_hint,
                init,
                ..
            } => {
                self.check_let_stmt(name, type_hint.as_deref(), *init, arena, symbols);
            }
            StmtKind::Assign { name, value } => {
                self.check_assign_stmt(name, *value, arena, symbols);
            }
            StmtKind::Expr(expr_id) => {
                self.check_expr(*expr_id, arena, symbols);
            }
            StmtKind::Return {
                value: Some(expr_id),
            } => {
                self.check_return_stmt(*expr_id, arena, symbols);
            }
            StmtKind::Return { value: None } => {}
            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.check_if_stmt(*condition, then_branch, else_branch, arena, symbols);
            }
            StmtKind::For {
                var,
                iterable,
                body,
                ..
            } => {
                self.check_for_stmt(var, *iterable, body, arena, symbols);
            }
            StmtKind::IndexAssign {
                object,
                index,
                value,
                ..
            } => {
                self.check_index_assign_stmt(*object, *index, *value, arena, symbols);
            }
            StmtKind::TaskDef {
                name,
                params,
                return_type,
                body,
                ..
            } => {
                self.check_task_def_stmt(
                    name,
                    params,
                    return_type.as_deref(),
                    body,
                    arena,
                    symbols,
                );
            }
            StmtKind::Match { expr, arms } => {
                self.check_match_stmt(*expr, arms, arena, symbols);
            }
            StmtKind::With { bindings, body } => {
                self.check_with_stmt(bindings, body, arena, symbols);
            }
            StmtKind::Parallel { stmts } | StmtKind::MacroDef { body: stmts, .. } => {
                self.check_block_stmts(stmts, arena, symbols);
            }
            StmtKind::Worker { body, .. } => {
                self.check_block_stmts(body, arena, symbols);
            }
            StmtKind::Transaction { body, compensation } => {
                self.check_transaction_stmt(body, compensation, arena, symbols);
            }
            StmtKind::StreamFor { prompt, body, .. } => {
                self.check_stream_for_stmt(*prompt, body, arena, symbols);
            }
            StmtKind::ToolDef { body, .. } | StmtKind::Observe { body, .. } => {
                self.check_block_stmts(body, arena, symbols);
            }
            StmtKind::Span {
                attributes, body, ..
            } => {
                self.check_span_stmt(attributes, body, arena, symbols);
            }
            StmtKind::Send { value, .. } | StmtKind::Route { target: value, .. } => {
                self.check_expr(*value, arena, symbols);
            }
            StmtKind::RecordTokens { input, output } => {
                self.check_expr(*input, arena, symbols);
                self.check_expr(*output, arena, symbols);
            }
            StmtKind::Save { path, value } => {
                self.check_expr(*path, arena, symbols);
                self.check_expr(*value, arena, symbols);
            }
            StmtKind::Load { path, var, .. } => {
                self.check_expr(*path, arena, symbols);
                symbols.define(var.clone(), Type::Union(vec![]));
            }
            StmtKind::ReadFile { path, var, .. } | StmtKind::ReadBytesFile { path, var, .. } => {
                self.check_expr(*path, arena, symbols);
                symbols.define(var.clone(), Type::String);
            }
            StmtKind::WriteFile { path, content, .. }
            | StmtKind::AppendFile { path, content, .. }
            | StmtKind::WriteBytesFile { path, content, .. } => {
                self.check_expr(*path, arena, symbols);
                self.check_expr(*content, arena, symbols);
            }
            StmtKind::TraitDef { name, methods, .. } => {
                self.check_trait_def_stmt(name, methods, symbols);
            }
            StmtKind::ImplDef {
                trait_name,
                for_type,
                methods,
                ..
            } => {
                self.check_impl_def_stmt(trait_name, for_type, methods, arena, symbols);
            }
            StmtKind::TypeAlias { name, target, .. } => {
                symbols.define(name.clone(), Type::from_hint(target));
            }
            StmtKind::EnumDef { name, variants, .. } => {
                self.check_enum_def_stmt(name, variants, symbols);
            }
            StmtKind::StructDef { name, fields, .. } => {
                self.check_struct_def_stmt(name, fields, symbols);
            }
            // No-ops
            StmtKind::Import { .. }
            | StmtKind::Receive { .. }
            | StmtKind::Commit
            | StmtKind::Rollback
            | StmtKind::Break
            | StmtKind::Continue => {}
            StmtKind::Orchestrate { kind, .. } => {
                self.check_orchestrate_stmt(kind, arena, symbols);
            }
            StmtKind::Eval { given, expects, .. } => {
                self.check_eval_stmt(*given, expects, arena, symbols);
            }
            StmtKind::SkillDef { tasks, verify, .. } => {
                self.check_skill_def_stmt(tasks, verify.as_ref(), arena, symbols);
            }
        }
    }

    /// 检查 let 语句
    fn check_let_stmt(
        &mut self,
        name: &str,
        type_hint: Option<&str>,
        init: NodeId,
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        let init_ty = self.check_expr(init, arena, symbols);
        let declared = if let Some(hint) = type_hint {
            let t = Type::from_hint(hint);
            // 检查未知类型名
            if !is_known_type(hint) && !self.trait_registry.contains_key(hint) {
                self.errors.push(TypeError {
                    message: format!("unknown type name '{}'", hint),
                    line: 0,
                    column: 0,
                    expected: None,
                    actual: Some(hint.to_string()),
                    hint: Some("check the type name spelling".to_string()),
                });
            }
            // 检查兼容性
            if init_ty != Type::Union(vec![]) && !self.types_compatible(&t, &init_ty) {
                self.errors.push(TypeError {
                    message: format!(
                        "type mismatch: expected '{}', got '{}'",
                        t.name(),
                        init_ty.name()
                    ),
                    line: 0,
                    column: 0,
                    expected: Some(t.name()),
                    actual: Some(init_ty.name()),
                    hint: Some("ensure the value matches the declared type".to_string()),
                });
            }
            t
        } else {
            init_ty
        };
        symbols.define(name.to_string(), declared);
    }

    /// 检查赋值语句
    fn check_assign_stmt(
        &mut self,
        name: &str,
        value: NodeId,
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        let _ty = self.check_expr(value, arena, symbols);
        let existing = symbols.lookup(name);
        if existing == Type::Union(vec![]) {
            self.errors.push(TypeError {
                message: format!("Undefined variable: {}", name),
                line: 0,
                column: 0,
                expected: None,
                actual: None,
                hint: Some(format!("let {} = ...", name)),
            });
        }
    }

    /// 检查 return 语句
    fn check_return_stmt(&mut self, expr_id: NodeId, arena: &AstArena, symbols: &mut SymbolTable) {
        let ret_ty = self.check_expr(expr_id, arena, symbols);
        if let Some(ref hint) = self.current_return_hint
            && ret_ty != Type::Union(vec![])
            && *hint != Type::Union(vec![])
            && !self.types_compatible(hint, &ret_ty)
        {
            self.errors.push(TypeError {
                message: format!(
                    "return type mismatch: expected '{}', got '{}'",
                    hint.name(),
                    ret_ty.name()
                ),
                line: 0,
                column: 0,
                expected: Some(hint.name()),
                actual: Some(ret_ty.name()),
                hint: Some("ensure the return value matches the declared type".to_string()),
            });
        }
    }

    /// 检查 if 语句
    fn check_if_stmt(
        &mut self,
        condition: NodeId,
        then_branch: &[NodeId],
        else_branch: &[NodeId],
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        let cond_ty = self.check_expr(condition, arena, symbols);
        if cond_ty != Type::Bool && cond_ty != Type::Union(vec![]) {
            self.errors.push(TypeError {
                message: format!("If condition must be bool, got {:?}", cond_ty),
                line: 0,
                column: 0,
                expected: Some("bool".to_string()),
                actual: Some(format!("{:?}", cond_ty)),
                hint: None,
            });
        }
        symbols.push_scope();
        for stmt_id in then_branch {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                self.check_stmt(&stmt.kind, arena, symbols);
            }
        }
        symbols.pop_scope();

        symbols.push_scope();
        for stmt_id in else_branch {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                self.check_stmt(&stmt.kind, arena, symbols);
            }
        }
        symbols.pop_scope();
    }

    /// 检查 for 语句
    fn check_for_stmt(
        &mut self,
        var: &str,
        iterable: NodeId,
        body: &[NodeId],
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        let iterable_ty = self.check_expr(iterable, arena, symbols);
        symbols.push_scope();
        // 推断循环变量类型
        match &iterable_ty {
            Type::List(inner) => symbols.define(var.to_string(), *inner.clone()),
            Type::String => symbols.define(var.to_string(), Type::Char),
            Type::Union(_) => symbols.define(var.to_string(), Type::Union(vec![])),
            _ => {
                self.errors.push(TypeError {
                    message: format!(
                        "for loop expects a list or string, got '{}'",
                        iterable_ty.name()
                    ),
                    line: 0,
                    column: 0,
                    expected: Some("list | string".to_string()),
                    actual: Some(iterable_ty.name()),
                    hint: Some("convert to list or string first".to_string()),
                });
                symbols.define(var.to_string(), Type::Union(vec![]));
            }
        }
        for stmt_id in body {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                self.check_stmt(&stmt.kind, arena, symbols);
            }
        }
        symbols.pop_scope();
    }

    /// 检查索引赋值语句
    fn check_index_assign_stmt(
        &mut self,
        object: NodeId,
        index: NodeId,
        value: NodeId,
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        let obj_ty = self.check_expr(object, arena, symbols);
        self.check_expr(index, arena, symbols);
        let val_ty = self.check_expr(value, arena, symbols);
        // 检查赋值类型兼容性
        if let Type::List(elem_ty) = &obj_ty
            && val_ty != Type::Union(vec![])
            && !self.types_compatible(elem_ty, &val_ty)
        {
            self.errors.push(TypeError {
                message: format!(
                    "element type mismatch on assign: expected '{}', got '{}'",
                    elem_ty.name(),
                    val_ty.name()
                ),
                line: 0,
                column: 0,
                expected: Some(elem_ty.name()),
                actual: Some(val_ty.name()),
                hint: Some("ensure the value matches the list element type".to_string()),
            });
        }
    }

    /// 检查函数定义语句
    fn check_task_def_stmt(
        &mut self,
        name: &str,
        params: &[(String, Option<String>)],
        return_type: Option<&str>,
        body: &[NodeId],
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        symbols.push_scope();
        for (pname, phint) in params {
            let pty = phint
                .as_deref()
                .map(Type::from_hint)
                .unwrap_or(Type::Union(vec![]));
            symbols.define(pname.clone(), pty);
        }
        self.current_return_hint = return_type
            .map(Type::from_hint)
            .or(Some(Type::Union(vec![])));
        for stmt_id in body {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                let kind = stmt.kind.clone();
                self.check_stmt(&kind, arena, symbols);
            }
        }
        self.current_return_hint = None;
        symbols.pop_scope();
        // 注册函数签名
        let param_types: Vec<(String, Type)> = params
            .iter()
            .map(|(n, hint)| {
                (
                    n.clone(),
                    hint.as_deref()
                        .map(Type::from_hint)
                        .unwrap_or(Type::Union(vec![])),
                )
            })
            .collect();
        let raw_params: Vec<Option<String>> = params.iter().map(|(_, hint)| hint.clone()).collect();
        let ret = return_type
            .map(Type::from_hint)
            .unwrap_or(Type::Union(vec![]));
        let raw_ret = return_type.map(|s| s.to_string());
        self.signatures.insert(
            name.to_string(),
            Signature {
                params: param_types,
                raw_params,
                return_type: ret,
                raw_return_type: raw_ret,
            },
        );
    }

    /// 检查模式匹配语句
    fn check_match_stmt(
        &mut self,
        expr: NodeId,
        arms: &[(crate::ast_v2::Pattern, Vec<NodeId>)],
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        self.check_expr(expr, arena, symbols);
        for (_pattern, arm_stmts) in arms {
            symbols.push_scope();
            for stmt_id in arm_stmts {
                if let Some(stmt) = arena.get_stmt(*stmt_id) {
                    let kind = stmt.kind.clone();
                    self.check_stmt(&kind, arena, symbols);
                }
            }
            symbols.pop_scope();
        }
    }

    /// 检查 with 语句
    fn check_with_stmt(
        &mut self,
        bindings: &[(String, NodeId)],
        body: &[NodeId],
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        for (_, expr_id) in bindings {
            self.check_expr(*expr_id, arena, symbols);
        }
        for stmt_id in body {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                let kind = stmt.kind.clone();
                self.check_stmt(&kind, arena, symbols);
            }
        }
    }

    /// 检查代码块中的语句
    fn check_block_stmts(&mut self, stmts: &[NodeId], arena: &AstArena, symbols: &mut SymbolTable) {
        for stmt_id in stmts {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                let kind = stmt.kind.clone();
                self.check_stmt(&kind, arena, symbols);
            }
        }
    }

    /// 检查事务语句
    fn check_transaction_stmt(
        &mut self,
        body: &[NodeId],
        compensation: &[NodeId],
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        for stmt_id in body {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                let kind = stmt.kind.clone();
                self.check_stmt(&kind, arena, symbols);
            }
        }
        for stmt_id in compensation {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                let kind = stmt.kind.clone();
                self.check_stmt(&kind, arena, symbols);
            }
        }
    }

    /// 检查 stream for 语句
    fn check_stream_for_stmt(
        &mut self,
        prompt: NodeId,
        body: &[NodeId],
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        self.check_expr(prompt, arena, symbols);
        for stmt_id in body {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                let kind = stmt.kind.clone();
                self.check_stmt(&kind, arena, symbols);
            }
        }
    }

    /// 检查 span 语句
    fn check_span_stmt(
        &mut self,
        attributes: &[(String, NodeId)],
        body: &[NodeId],
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        for (_, expr_id) in attributes {
            self.check_expr(*expr_id, arena, symbols);
        }
        for stmt_id in body {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                let kind = stmt.kind.clone();
                self.check_stmt(&kind, arena, symbols);
            }
        }
    }

    /// 检查 trait 定义语句
    fn check_trait_def_stmt(
        &mut self,
        name: &str,
        methods: &[crate::ast_v2::TraitMethod],
        _symbols: &mut SymbolTable,
    ) {
        let mut seen = std::collections::HashSet::new();
        for m in methods {
            if !seen.insert(&m.name) {
                self.errors.push(TypeError::from_span(
                    &m.span,
                    format!("trait '{}': duplicate method '{}'", name, m.name),
                ));
            }
        }
    }

    /// 检查 impl 定义语句
    fn check_impl_def_stmt(
        &mut self,
        trait_name: &str,
        for_type: &str,
        methods: &[crate::ast_v2::FnDef],
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        if !self.trait_registry.contains_key(trait_name) {
            self.errors.push(TypeError::from_span(
                &Span::default(),
                format!("impl: trait '{}' not defined", trait_name),
            ));
        }
        for m in methods {
            symbols.push_scope();
            for (pname, phint) in &m.params {
                let pty = phint
                    .as_deref()
                    .map(Type::from_hint)
                    .unwrap_or(Type::Union(vec![]));
                symbols.define(pname.clone(), pty);
            }
            for stmt_id in &m.body {
                if let Some(stmt) = arena.get_stmt(*stmt_id) {
                    let kind = stmt.kind.clone();
                    self.check_stmt(&kind, arena, symbols);
                }
            }
            symbols.pop_scope();
        }
        symbols.define(
            for_type.to_string(),
            Type::Trait {
                name: trait_name.to_string(),
                generics: vec![],
            },
        );
    }

    /// 检查枚举定义语句
    fn check_enum_def_stmt(
        &mut self,
        name: &str,
        variants: &[crate::common::EnumVariant],
        symbols: &mut SymbolTable,
    ) {
        symbols.define(
            name.to_string(),
            Type::Dict(Box::new(Type::String), Box::new(Type::Union(vec![]))),
        );
        for v in variants {
            symbols.define(v.name.clone(), Type::Builtin);
        }
    }

    /// 检查结构体定义语句
    fn check_struct_def_stmt(
        &mut self,
        name: &str,
        fields: &[crate::common::StructField],
        symbols: &mut SymbolTable,
    ) {
        let param_types: Vec<(String, Type)> = fields
            .iter()
            .map(|f| (f.name.clone(), Type::from_hint(&f.type_hint)))
            .collect();
        self.signatures.insert(
            name.to_string(),
            Signature {
                params: param_types,
                raw_params: fields.iter().map(|f| Some(f.type_hint.clone())).collect(),
                return_type: Type::Task,
                raw_return_type: Some(name.to_string()),
            },
        );
        symbols.define(name.to_string(), Type::Task);
    }

    /// 检查 orchestrate 语句
    fn check_orchestrate_stmt(
        &mut self,
        kind: &crate::ast_v2::OrchestrateKind,
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        match kind {
            crate::ast_v2::OrchestrateKind::Sequential { agents }
            | crate::ast_v2::OrchestrateKind::Graph { agents, .. } => {
                for agent in agents {
                    self.check_expr(agent.task_expr, arena, symbols);
                    if let Some(ref verify) = agent.verify_expr {
                        self.check_expr(*verify, arena, symbols);
                    }
                }
            }
            crate::ast_v2::OrchestrateKind::Loop {
                agent, exit_when, ..
            } => {
                self.check_expr(agent.task_expr, arena, symbols);
                if let Some(ref verify) = agent.verify_expr {
                    self.check_expr(*verify, arena, symbols);
                }
                if let Some(cond) = exit_when {
                    self.check_expr(*cond, arena, symbols);
                }
            }
        }
    }

    /// 检查 eval 语句
    fn check_eval_stmt(
        &mut self,
        given: NodeId,
        expects: &[NodeId],
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        self.check_expr(given, arena, symbols);
        for expect_id in expects {
            self.check_expr(*expect_id, arena, symbols);
        }
    }

    /// 检查 skill 定义语句
    fn check_skill_def_stmt(
        &mut self,
        tasks: &[crate::ast_v2::SkillTask],
        verify: Option<&crate::ast_v2::SkillVerify>,
        arena: &AstArena,
        symbols: &mut SymbolTable,
    ) {
        for task in tasks {
            symbols.push_scope();
            for (pname, phint) in &task.params {
                let pty = phint
                    .as_deref()
                    .map(Type::from_hint)
                    .unwrap_or(Type::Union(vec![]));
                symbols.define(pname.clone(), pty);
            }
            for stmt_id in &task.body {
                if let Some(stmt) = arena.get_stmt(*stmt_id) {
                    let kind = stmt.kind.clone();
                    self.check_stmt(&kind, arena, symbols);
                }
            }
            symbols.pop_scope();
        }
        if let Some(v) = verify {
            symbols.push_scope();
            for (pname, phint) in &v.params {
                let pty = phint
                    .as_deref()
                    .map(Type::from_hint)
                    .unwrap_or(Type::Union(vec![]));
                symbols.define(pname.clone(), pty);
            }
            for stmt_id in &v.body {
                if let Some(stmt) = arena.get_stmt(*stmt_id) {
                    let kind = stmt.kind.clone();
                    self.check_stmt(&kind, arena, symbols);
                }
            }
            symbols.pop_scope();
        }
    }

    /// 检查表达式
    pub fn check_expr(&mut self, expr_id: NodeId, arena: &AstArena, symbols: &SymbolTable) -> Type {
        let expr = match arena.get_expr(expr_id) {
            Some(e) => e,
            None => return Type::Union(vec![]),
        };
        match &expr.kind {
            ExprKind::Literal(lit) => match lit {
                crate::common::Literal::String(..) => Type::String,
                crate::common::Literal::Char(..) => Type::Char,
                crate::common::Literal::Number(..) => Type::Number,
                crate::common::Literal::Bool(..) => Type::Bool,
                crate::common::Literal::Nil(..) => Type::Nil,
            },
            ExprKind::Variable(name) => symbols.lookup(name),
            ExprKind::Binary { left, op, right } => {
                self.check_binary_expr(*left, op, *right, arena, symbols)
            }
            ExprKind::Call { callee, args } => self.check_call_expr(callee, args, arena, symbols),
            ExprKind::Grouping(inner) => self.check_expr(*inner, arena, symbols),
            ExprKind::Index { object, index } => {
                self.check_index_expr(*object, *index, expr, arena, symbols)
            }
            ExprKind::List(items) => self.check_list_expr(items, expr, arena, symbols),
            ExprKind::Dict(entries) => self.check_dict_expr(entries, expr, arena, symbols),
            ExprKind::Prompt { parts } => self.check_prompt_expr(parts, arena, symbols),
            ExprKind::Pipe { left, right } => self.check_pipe_expr(*left, *right, arena, symbols),
            ExprKind::Borrow { expr: inner } | ExprKind::BorrowMut { expr: inner } => {
                self.check_expr(*inner, arena, symbols)
            }
            ExprKind::NamespaceRef { .. } => Type::Union(vec![]),
            ExprKind::DynTrait { trait_name, .. } => Type::Trait {
                name: trait_name.clone(),
                generics: vec![],
            },
            ExprKind::Question { expr } => self.check_question_expr(*expr, arena, symbols),
            ExprKind::MethodCall {
                object,
                method,
                args,
            } => self.check_method_call_expr(*object, method, args, arena, symbols),
            _ => Type::Union(vec![]),
        }
    }

    /// 检查二元表达式
    fn check_binary_expr(
        &mut self,
        left: NodeId,
        op: &BinaryOp,
        right: NodeId,
        arena: &AstArena,
        symbols: &SymbolTable,
    ) -> Type {
        let left_ty = self.check_expr(left, arena, symbols);
        let right_ty = self.check_expr(right, arena, symbols);
        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => {
                if left_ty == Type::Number && right_ty == Type::Number {
                    Type::Number
                } else if left_ty == Type::String || right_ty == Type::String {
                    Type::String
                } else {
                    if left_ty != Type::Union(vec![]) && right_ty != Type::Union(vec![]) {
                        self.errors.push(TypeError {
                            message: format!(
                                "type mismatch: operator not defined for '{}' and '{}'",
                                left_ty.name(),
                                right_ty.name()
                            ),
                            line: 0,
                            column: 0,
                            expected: Some("number | string".to_string()),
                            actual: Some(format!("{} | {}", left_ty.name(), right_ty.name())),
                            hint: Some(
                                "arithmetic operators require number or string operands"
                                    .to_string(),
                            ),
                        });
                    }
                    Type::Union(vec![])
                }
            }
            BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Greater
            | BinaryOp::Less
            | BinaryOp::GreaterEqual
            | BinaryOp::LessEqual => Type::Bool,
        }
    }

    /// 检查函数调用表达式
    fn check_call_expr(
        &mut self,
        callee: &str,
        args: &[NodeId],
        arena: &AstArena,
        symbols: &SymbolTable,
    ) -> Type {
        let arg_types: Vec<Type> = args
            .iter()
            .map(|id| self.check_expr(*id, arena, symbols))
            .collect();
        // 检查函数签名
        if let Some(sig) = self.signatures.get(callee) {
            // 检查参数数量
            if !sig.params.is_empty() && arg_types.len() != sig.params.len() {
                self.errors.push(TypeError {
                    message: format!(
                        "'{}' expects {} args, got {}",
                        callee,
                        sig.params.len(),
                        arg_types.len()
                    ),
                    line: 0,
                    column: 0,
                    expected: Some(format!("{} args", sig.params.len())),
                    actual: Some(format!("{} args", arg_types.len())),
                    hint: None,
                });
            }
            // 检查参数类型
            for (i, (expected, actual)) in sig.params.iter().zip(arg_types.iter()).enumerate() {
                if actual != &Type::Union(vec![])
                    && expected.1 != Type::Union(vec![])
                    && !self.types_compatible(&expected.1, actual)
                {
                    self.errors.push(TypeError {
                        message: format!(
                            "'{}' param {} type mismatch: expected '{}', got '{}'",
                            callee,
                            i,
                            expected.1.name(),
                            actual.name()
                        ),
                        line: 0,
                        column: 0,
                        expected: Some(expected.1.name()),
                        actual: Some(actual.name()),
                        hint: None,
                    });
                }
            }
            sig.return_type.clone()
        } else {
            Type::Union(vec![])
        }
    }

    /// 检查索引表达式
    fn check_index_expr(
        &mut self,
        object: NodeId,
        index: NodeId,
        expr: &crate::ast_v2::TypedExpr,
        arena: &AstArena,
        symbols: &SymbolTable,
    ) -> Type {
        let ot = self.check_expr(object, arena, symbols);
        let it = self.check_expr(index, arena, symbols);
        match &ot {
            Type::List(elem) => {
                if !matches!(&it, Type::Number) {
                    self.errors.push(TypeError::from_span_with_detail(
                        &expr.span,
                        "list index must be number",
                        "number",
                        it.name(),
                        "use a number to index a list",
                    ));
                }
                elem.as_ref().clone()
            }
            Type::Dict(_k, v) => {
                if !matches!(&it, Type::String) {
                    self.errors.push(TypeError::from_span_with_detail(
                        &expr.span,
                        "dict key must be string",
                        "string",
                        it.name(),
                        "use a string key to index a dict",
                    ));
                }
                v.as_ref().clone()
            }
            Type::String => {
                if !matches!(&it, Type::Number) {
                    self.errors.push(TypeError::from_span_with_detail(
                        &expr.span,
                        "string index must be number",
                        "number",
                        it.name(),
                        "use a number to index a string",
                    ));
                }
                Type::Char
            }
            Type::Union(_) => Type::Union(vec![]),
            _ => {
                self.errors.push(TypeError::from_span_with_detail(
                    &expr.span,
                    format!("cannot index type '{}'", ot.name()),
                    "list | dict | string",
                    ot.name(),
                    "use a container type",
                ));
                Type::Union(vec![])
            }
        }
    }

    /// 检查列表表达式
    fn check_list_expr(
        &mut self,
        items: &[NodeId],
        expr: &crate::ast_v2::TypedExpr,
        arena: &AstArena,
        symbols: &SymbolTable,
    ) -> Type {
        if items.is_empty() {
            return Type::List(Box::new(Type::Union(vec![])));
        }
        let first_ty = self.check_expr(items[0], arena, symbols);
        for (i, item_id) in items.iter().enumerate().skip(1) {
            let ity = self.check_expr(*item_id, arena, symbols);
            if is_empty_union(&first_ty) || is_empty_union(&ity) {
                continue;
            }
            if !first_ty.compatible_with(&ity) || !ity.compatible_with(&first_ty) {
                self.errors.push(TypeError::from_span_with_detail(
                    &expr.span,
                    format!(
                        "list element type mismatch at index {}: expected '{}', got '{}'",
                        i,
                        first_ty.name(),
                        ity.name()
                    ),
                    first_ty.name(),
                    ity.name(),
                    "ensure all elements share the same type",
                ));
                return Type::List(Box::new(Type::Union(vec![])));
            }
        }
        Type::List(Box::new(first_ty))
    }

    /// 检查字典表达式
    fn check_dict_expr(
        &mut self,
        entries: &[(String, NodeId)],
        expr: &crate::ast_v2::TypedExpr,
        arena: &AstArena,
        symbols: &SymbolTable,
    ) -> Type {
        if entries.is_empty() {
            return Type::Dict(Box::new(Type::String), Box::new(Type::Union(vec![])));
        }
        let first_v_ty = self.check_expr(entries[0].1, arena, symbols);
        for (i, (_, val_id)) in entries.iter().enumerate().skip(1) {
            let vty = self.check_expr(*val_id, arena, symbols);
            if is_empty_union(&first_v_ty) || is_empty_union(&vty) {
                continue;
            }
            if !first_v_ty.compatible_with(&vty) || !vty.compatible_with(&first_v_ty) {
                self.errors.push(TypeError::from_span_with_detail(
                    &expr.span,
                    format!(
                        "dict value type mismatch at entry {}: expected '{}', got '{}'",
                        i,
                        first_v_ty.name(),
                        vty.name()
                    ),
                    first_v_ty.name(),
                    vty.name(),
                    "ensure all dict values share the same type",
                ));
                return Type::Dict(Box::new(Type::String), Box::new(Type::Union(vec![])));
            }
        }
        Type::Dict(Box::new(Type::String), Box::new(first_v_ty))
    }

    /// 检查模板字符串表达式
    fn check_prompt_expr(
        &mut self,
        parts: &[NodeId],
        arena: &AstArena,
        symbols: &SymbolTable,
    ) -> Type {
        for part_id in parts {
            self.check_expr(*part_id, arena, symbols);
        }
        Type::String
    }

    /// 检查管道表达式
    fn check_pipe_expr(
        &mut self,
        left: NodeId,
        right: NodeId,
        arena: &AstArena,
        symbols: &SymbolTable,
    ) -> Type {
        self.check_expr(left, arena, symbols);
        self.check_expr(right, arena, symbols)
    }

    /// 检查错误传播表达式
    fn check_question_expr(
        &mut self,
        expr: NodeId,
        arena: &AstArena,
        symbols: &SymbolTable,
    ) -> Type {
        let ty = self.check_expr(expr, arena, symbols);
        match ty {
            Type::Result_(inner, _) => *inner,
            _ => ty,
        }
    }

    /// 检查方法调用表达式
    fn check_method_call_expr(
        &mut self,
        object: NodeId,
        method: &str,
        args: &[NodeId],
        arena: &AstArena,
        symbols: &SymbolTable,
    ) -> Type {
        let obj_ty = self.check_expr(object, arena, symbols);
        // 检查 closure 参数类型注解
        if let Type::List(elem_ty) = &obj_ty {
            for arg_id in args {
                if let Some(arg_expr) = arena.get_expr(*arg_id)
                    && let ExprKind::Closure { params, .. } = &arg_expr.kind
                {
                    for (pname, phint) in params {
                        if phint.is_none() {
                            self.errors.push(TypeError {
                                message: format!(
                                    "missing type annotation for closure parameter '{}'",
                                    pname
                                ),
                                line: 0,
                                column: 0,
                                expected: Some(elem_ty.name()),
                                actual: Some("unknown".to_string()),
                                hint: Some(format!(
                                    "add type annotation: fn({}: {})",
                                    pname,
                                    elem_ty.name()
                                )),
                            });
                        }
                    }
                }
            }
        }
        for arg_id in args {
            self.check_expr(*arg_id, arena, symbols);
        }
        // 使用 method_return_type 推断返回类型（保留元素类型）
        method_return_type(&obj_ty, method)
    }

    /// 检查类型兼容性
    fn types_compatible(&self, a: &Type, b: &Type) -> bool {
        if a == b {
            return true;
        }
        matches!((a, b), (Type::Union(_), _) | (_, Type::Union(_)))
    }
}
