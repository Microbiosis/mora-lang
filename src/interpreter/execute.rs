//! 语句执行模块
//!
//! 从 interpreter/mod.rs 提取的 execute 函数拆分为多个小函数

use super::*;
use crate::ast_v2::{AstArena, NodeId, StmtKind};
use crate::value::{Environment, FlowSignal, Value};

impl Interpreter {
    /// 执行语句（主入口）
    pub fn execute(
        &mut self,
        stmt_kind: &StmtKind,
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        match stmt_kind {
            StmtKind::Let {
                name,
                init,
                exported,
                ..
            } => self.execute_let(name, *init, *exported, arena),
            StmtKind::Assign { name, value } => self.execute_assign(name, *value, arena),
            StmtKind::Expr(expr_id) => {
                self.evaluate(*expr_id, arena)?;
                Ok(FlowSignal::None)
            }
            StmtKind::Return { value } => self.execute_return(*value, arena),
            StmtKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.execute_if(*condition, then_branch, else_branch, arena),
            StmtKind::For {
                var,
                iterable,
                body,
                ..
            } => self.execute_for(var, *iterable, body, arena),
            StmtKind::Import { path } => self.execute_import(path),
            StmtKind::Break => Ok(FlowSignal::Break),
            StmtKind::Continue => Ok(FlowSignal::Continue),
            StmtKind::TaskDef {
                name,
                params,
                body,
                exported,
                ..
            } => self.execute_task_def(name, params, body, *exported, arena),
            StmtKind::Match { expr, arms } => self.execute_match(*expr, arms, arena),
            StmtKind::With { bindings, body } => self.execute_with(bindings, body, arena),
            StmtKind::Parallel { stmts } => self.execute_parallel(stmts, arena),
            StmtKind::Worker { .. } => Ok(FlowSignal::None),
            StmtKind::Send { value, target } => self.execute_send(*value, target, arena),
            StmtKind::Receive { var, source } => self.execute_receive(var, source, arena),
            StmtKind::Transaction { body, compensation } => {
                self.execute_transaction(body, compensation, arena)
            }
            StmtKind::Commit => Ok(FlowSignal::None),
            StmtKind::Rollback => Err("Transaction rolled back".to_string()),
            StmtKind::MacroDef {
                name,
                params,
                body: _,
            } => self.execute_macro_def(name, params, arena),
            StmtKind::TypeAlias { name, target, .. } => {
                self.execute_type_alias(name, target, arena)
            }
            StmtKind::EnumDef { name, variants, .. } => {
                self.execute_enum_def(name, variants, arena)
            }
            StmtKind::StructDef { name, fields, .. } => {
                self.execute_struct_def(name, fields, arena)
            }
            StmtKind::TraitDef {
                name,
                generics: _,
                parents,
                methods,
                ..
            } => self.execute_trait_def(name, parents, methods, arena),
            StmtKind::ImplDef {
                trait_generics,
                trait_name,
                for_type,
                for_generics,
                methods,
                ..
            } => self.execute_impl_def(
                trait_name,
                trait_generics,
                for_type,
                for_generics,
                methods.as_slice(),
                arena,
            ),
            StmtKind::Orchestrate {
                input_var,
                result_var,
                kind,
            } => self.execute_orchestrate(input_var, result_var, kind, arena),
            StmtKind::Eval {
                name,
                given,
                expects,
                tolerance,
                replay_path,
            } => self.execute_eval(
                name,
                *given,
                expects,
                *tolerance,
                replay_path.as_deref(),
                arena,
            ),
            StmtKind::SkillDef {
                name,
                description,
                version,
                requires,
                tasks,
                verify,
            } => self.execute_skill_def_impl(
                name,
                description,
                version,
                requires,
                tasks,
                verify,
                arena,
            ),
            _ => Err(format!("Unsupported v2 statement: {:?}", stmt_kind)),
        }
    }

    /// 执行 let 绑定
    fn execute_let(
        &mut self,
        name: &str,
        init: NodeId,
        exported: bool,
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let value = self.evaluate(init, arena)?;
        self.environment
            .lock()
            .expect("environment mutex poisoned")
            .define(name.to_string(), value, exported);
        Ok(FlowSignal::None)
    }

    /// 执行赋值
    fn execute_assign(
        &mut self,
        name: &str,
        value: NodeId,
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let val = self.evaluate(value, arena)?;
        if !self
            .environment
            .lock()
            .expect("env mutex poisoned")
            .assign(name, val.clone())
        {
            return Err(format!("Undefined variable: {}", name));
        }
        Ok(FlowSignal::None)
    }

    /// 执行 return 语句
    fn execute_return(
        &mut self,
        value: Option<NodeId>,
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let val = match value {
            Some(id) => self.evaluate(id, arena)?,
            None => Value::Nil,
        };
        Ok(FlowSignal::Return(val))
    }

    /// 执行 if 语句
    fn execute_if(
        &mut self,
        condition: NodeId,
        then_branch: &[NodeId],
        else_branch: &[NodeId],
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let cond = self.evaluate(condition, arena)?;
        if is_truthy(&cond) {
            for stmt_id in then_branch {
                if let Some(stmt) = arena.get_stmt(*stmt_id) {
                    let kind = stmt.kind.clone();
                    match self.execute(&kind, arena)? {
                        FlowSignal::None => {}
                        signal => return Ok(signal),
                    }
                }
            }
        } else {
            for stmt_id in else_branch {
                if let Some(stmt) = arena.get_stmt(*stmt_id) {
                    let kind = stmt.kind.clone();
                    match self.execute(&kind, arena)? {
                        FlowSignal::None => {}
                        signal => return Ok(signal),
                    }
                }
            }
        }
        Ok(FlowSignal::None)
    }

    /// 执行 for 循环
    fn execute_for(
        &mut self,
        var: &str,
        iterable: NodeId,
        body: &[NodeId],
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let iterable_val = self.evaluate(iterable, arena)?;
        let items: Vec<Value> = match iterable_val {
            Value::List(items) => items,
            Value::String(s) => s.chars().map(Value::Char).collect(),
            _ => return Err("for loop requires a list or string".to_string()),
        };
        for item in items {
            self.environment.lock().expect("env mutex poisoned").define(
                var.to_string(),
                item,
                false,
            );
            for stmt_id in body {
                if let Some(stmt) = arena.get_stmt(*stmt_id) {
                    let kind = stmt.kind.clone();
                    match self.execute(&kind, arena)? {
                        FlowSignal::None => {}
                        FlowSignal::Break => return Ok(FlowSignal::None),
                        FlowSignal::Continue => break,
                        signal => return Ok(signal),
                    }
                }
            }
        }
        Ok(FlowSignal::None)
    }

    /// 执行 import 语句
    fn execute_import(&mut self, path: &str) -> Result<FlowSignal, String> {
        match std::fs::read_to_string(path) {
            Ok(source) => {
                let tokens = crate::lexer::Lexer::new(&source).scan_tokens();
                let mut parser_v2 = crate::parser_v2::ParserV2::new(tokens);
                let imported_ids = parser_v2.parse();
                let imported_arena = parser_v2.into_arena();
                for sid in &imported_ids {
                    if let Some(stmt) = imported_arena.get_stmt(*sid) {
                        let kind = stmt.kind.clone();
                        self.execute(&kind, &imported_arena)?;
                    }
                }
                Ok(FlowSignal::None)
            }
            Err(e) => Err(format!("import error: {}", e)),
        }
    }

    /// 执行 task 定义
    fn execute_task_def(
        &mut self,
        name: &str,
        params: &[(String, Option<String>)],
        body: &[NodeId],
        exported: bool,
        _arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
        let body_ids: Vec<usize> = body.iter().map(|id| id.0).collect();
        self.environment.lock().expect("env mutex poisoned").define(
            name.to_string(),
            Value::Task {
                name: name.to_string(),
                params: param_names,
                v2_body_ids: body_ids,
            },
            exported,
        );
        Ok(FlowSignal::None)
    }

    /// 执行 match 语句
    fn execute_match(
        &mut self,
        expr: NodeId,
        arms: &[(crate::ast_v2::Pattern, Vec<NodeId>)],
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let val = self.evaluate(expr, arena)?;
        for (pattern, body_ids) in arms {
            if let Some(bindings) = self.match_pattern(pattern, &val, arena) {
                let env = Arc::new(Mutex::new(Environment::with_parent(
                    self.environment.clone(),
                )));
                for (name, value) in bindings {
                    env.lock()
                        .expect("env mutex poisoned")
                        .define(name, value, false);
                }
                let previous = self.environment.clone();
                self.environment = env;
                let mut result = FlowSignal::None;
                for body_id in body_ids {
                    if let Some(stmt) = arena.get_stmt(*body_id) {
                        let kind = stmt.kind.clone();
                        result = self.execute(&kind, arena)?;
                        if !matches!(result, FlowSignal::None) {
                            break;
                        }
                    }
                }
                self.environment = previous;
                return Ok(result);
            }
        }
        Ok(FlowSignal::None)
    }

    /// 执行 with 块
    fn execute_with(
        &mut self,
        bindings: &[(String, NodeId)],
        body: &[NodeId],
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let prev_cfg = self.current_ai_config.clone();
        let mut cfg = prev_cfg.clone().unwrap_or_default();
        for (key, val_id) in bindings {
            let v = self.evaluate(*val_id, arena)?;
            match key.as_str() {
                "model" => cfg.model = Some(v.to_string()),
                "temperature" => {
                    if let Value::Number(n) = v {
                        cfg.temperature = Some(n);
                    }
                }
                "max_tokens" => {
                    if let Value::Number(n) = v {
                        cfg.max_tokens = Some(n as usize);
                    }
                }
                "system" => cfg.system = Some(v.to_string()),
                "mock_llm" => {
                    if let Value::List(items) = v {
                        cfg.mock_responses = Some(items.iter().map(|i| i.to_string()).collect());
                    }
                }
                "compact_at" => {
                    if let Value::Number(n) = v {
                        self.context_window.compression_threshold = n / 100.0;
                    }
                }
                _ => {}
            }
        }
        self.current_ai_config = Some(cfg);
        for stmt_id in body {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                let kind = stmt.kind.clone();
                match self.execute(&kind, arena)? {
                    FlowSignal::None => {}
                    signal => {
                        self.current_ai_config = prev_cfg;
                        return Ok(signal);
                    }
                }
            }
        }
        self.current_ai_config = prev_cfg;
        Ok(FlowSignal::None)
    }

    /// 执行 parallel 块
    fn execute_parallel(
        &mut self,
        stmts: &[NodeId],
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        // 简化实现：顺序执行
        for stmt_id in stmts {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                let kind = stmt.kind.clone();
                match self.execute(&kind, arena)? {
                    FlowSignal::None => {}
                    signal => return Ok(signal),
                }
            }
        }
        Ok(FlowSignal::None)
    }

    /// 执行 send 语句
    fn execute_send(
        &mut self,
        value: NodeId,
        target: &str,
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let val = self.evaluate(value, arena)?;
        if let Some(tx) = self.worker_channels.get(target) {
            tx.send(val).map_err(|e| format!("Send error: {}", e))?;
        }
        Ok(FlowSignal::None)
    }

    /// 执行 receive 语句
    fn execute_receive(
        &mut self,
        var: &str,
        source: &str,
        _arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        if let Some(rx) = self.worker_receivers.get(source) {
            let val = rx.recv().map_err(|e| format!("Receive error: {}", e))?;
            self.environment.lock().expect("env mutex poisoned").define(
                var.to_string(),
                val,
                false,
            );
        }
        Ok(FlowSignal::None)
    }

    /// 执行事务
    fn execute_transaction(
        &mut self,
        body: &[NodeId],
        compensation: &[NodeId],
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let mut result = FlowSignal::None;
        let mut error_occurred = false;
        for stmt_id in body {
            if let Some(stmt) = arena.get_stmt(*stmt_id) {
                let kind = stmt.kind.clone();
                match self.execute(&kind, arena) {
                    Ok(r) => {
                        result = r;
                        if !matches!(result, FlowSignal::None) {
                            error_occurred = true;
                            break;
                        }
                    }
                    Err(_) => {
                        error_occurred = true;
                        break;
                    }
                }
            }
        }
        // 执行补偿
        for comp_id in compensation {
            if let Some(stmt) = arena.get_stmt(*comp_id) {
                let kind = stmt.kind.clone();
                let _ = self.execute(&kind, arena);
            }
        }
        if error_occurred {
            return Err("Transaction rolled back".to_string());
        }
        Ok(result)
    }

    /// 执行宏定义
    fn execute_macro_def(
        &mut self,
        name: &str,
        params: &[String],
        _arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        self.environment.lock().expect("env mutex poisoned").define(
            name.to_string(),
            Value::Macro {
                name: name.to_string(),
                params: params.to_vec(),
            },
            false,
        );
        Ok(FlowSignal::None)
    }

    /// 执行类型别名
    fn execute_type_alias(
        &mut self,
        name: &str,
        target: &str,
        _arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        self.environment.lock().expect("env mutex poisoned").define(
            name.to_string(),
            Value::String(target.to_string()),
            false,
        );
        Ok(FlowSignal::None)
    }

    /// 执行枚举定义
    fn execute_enum_def(
        &mut self,
        name: &str,
        variants: &[crate::common::EnumVariant],
        _arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let mut enum_map = std::collections::HashMap::new();
        for v in variants {
            enum_map.insert(v.name.clone(), Value::Builtin(v.name.clone()));
        }
        self.environment.lock().expect("env mutex poisoned").define(
            name.to_string(),
            Value::Dict(enum_map),
            false,
        );
        Ok(FlowSignal::None)
    }

    /// 执行结构体定义
    fn execute_struct_def(
        &mut self,
        name: &str,
        fields: &[crate::common::StructField],
        _arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let field_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
        let constructor = Value::Closure {
            params: field_names,
            env: self.environment.clone(),
            v2_node_id: None,
        };
        self.environment.lock().expect("env mutex poisoned").define(
            name.to_string(),
            constructor,
            false,
        );
        Ok(FlowSignal::None)
    }

    /// 执行 trait 定义
    fn execute_trait_def(
        &mut self,
        name: &str,
        parents: &[String],
        methods: &[crate::ast_v2::TraitMethod],
        _arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let method_sigs: Vec<TraitMethodSig> = methods
            .iter()
            .map(|m| TraitMethodSig {
                name: m.name.clone(),
                params: m.params.clone(),
                return_type: m.return_type.clone(),
                has_self: m.params.first().map(|(n, _)| n == "self").unwrap_or(false),
            })
            .collect();
        self.trait_registry.insert(
            name.to_string(),
            TraitInfo {
                name: name.to_string(),
                parents: parents.to_vec(),
                methods: method_sigs,
            },
        );
        // 注册默认实现
        let trait_generics: Vec<String> = vec![]; // 无泛型时为空
        for m in methods {
            if !m.body.is_empty() {
                let body_ids: Vec<usize> = m.body.iter().map(|id| id.0).collect();
                let key = default_impl_method_key(name, &trait_generics, &m.name);
                self.environment.lock().expect("env").define(
                    key,
                    Value::Task {
                        name: m.name.clone(),
                        params: m.params.iter().map(|(n, _)| n.clone()).collect(),
                        v2_body_ids: body_ids,
                    },
                    false,
                );
            }
        }
        Ok(FlowSignal::None)
    }

    /// 执行 impl 定义
    fn execute_impl_def(
        &mut self,
        trait_name: &str,
        trait_generics: &[String],
        for_type: &str,
        for_generics: &[String],
        methods: &[crate::ast_v2::FnDef],
        _arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        let _method_names: Vec<String> = methods.iter().map(|m| m.name.clone()).collect();
        self.impl_table
            .entry(trait_name.to_string())
            .or_default()
            .push(for_type.to_string());
        // 注册每个方法到环境，key 使用标准格式
        for m in methods {
            let body_ids: Vec<usize> = m.body.iter().map(|id| id.0).collect();
            let key = impl_method_key(trait_name, trait_generics, for_type, for_generics, &m.name);
            self.environment.lock().expect("env").define(
                key,
                Value::Task {
                    name: m.name.clone(),
                    params: m.params.iter().map(|(n, _)| n.clone()).collect(),
                    v2_body_ids: body_ids,
                },
                false,
            );
        }
        Ok(FlowSignal::None)
    }

    /// 执行 eval 语句
    fn execute_eval(
        &mut self,
        name: &str,
        given: NodeId,
        expects: &[NodeId],
        tolerance: Option<f64>,
        replay_path: Option<&str>,
        arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        // 1. 求值 given
        let given_val = self.evaluate(given, arena)?;
        // 绑定到 `given` 变量供 expect 表达式使用
        self.environment
            .lock()
            .expect("env")
            .define("given".to_string(), given_val.clone(), false);

        // 2. 如果有 replay_path，切换到 replay 模式
        let prev_recorder = if let Some(path) = replay_path {
            let prev = std::mem::replace(
                &mut self.recorder,
                crate::record::Recorder::new_replay(std::path::PathBuf::from(path)).unwrap_or_else(
                    |e| {
                        eprintln!("eval replay warning: {}", e);
                        crate::record::Recorder::new_off()
                    },
                ),
            );
            Some(prev)
        } else {
            None
        };

        // 3. 求值每个 expect
        let mut passed = 0;
        let total = expects.len();
        for expect_id in expects {
            let result = self.evaluate(*expect_id, arena)?;
            match result {
                Value::Bool(true) => passed += 1,
                Value::Bool(false) => {}
                _ => {
                    // 非 bool 值视为 false
                }
            }
        }

        // 4. 恢复 recorder
        if let Some(prev) = prev_recorder {
            self.recorder = prev;
        }

        // 5. 计算通过率
        let pass_rate = if total > 0 {
            passed as f64 / total as f64
        } else {
            1.0
        };
        let tol = tolerance.unwrap_or(1.0);

        if pass_rate >= tol {
            // 通过
            self.environment.lock().expect("env").define(
                format!("eval_{}", name),
                Value::String(format!("PASS ({}/{})", passed, total)),
                false,
            );
            Ok(FlowSignal::None)
        } else {
            Err(format!(
                "eval '{}' failed: {}/{} passed (need {:.0}%)",
                name,
                passed,
                total,
                tol * 100.0
            ))
        }
    }

    /// 执行 skill 定义
    #[allow(clippy::too_many_arguments)]
    fn execute_skill_def_impl(
        &mut self,
        name: &str,
        description: &Option<String>,
        version: &Option<String>,
        requires: &[String],
        tasks: &[crate::ast_v2::SkillTask],
        verify: &Option<crate::ast_v2::SkillVerify>,
        _arena: &AstArena,
    ) -> Result<FlowSignal, String> {
        // 注册 Skill 元数据
        let mut skill_meta = std::collections::HashMap::new();
        skill_meta.insert("name".to_string(), Value::String(name.to_string()));
        if let Some(desc) = description {
            skill_meta.insert("description".to_string(), Value::String(desc.clone()));
        }
        if let Some(ver) = version {
            skill_meta.insert("version".to_string(), Value::String(ver.clone()));
        }
        let req_list: Vec<Value> = requires.iter().map(|r| Value::String(r.clone())).collect();
        skill_meta.insert("requires".to_string(), Value::List(req_list));

        // 将每个 task 存储为 Skill Dict 中的可调用值
        for task in tasks {
            let body_ids: Vec<usize> = task.body.iter().map(|id| id.0).collect();
            skill_meta.insert(
                task.name.clone(),
                Value::Task {
                    name: task.name.clone(),
                    params: task.params.iter().map(|(n, _)| n.clone()).collect(),
                    v2_body_ids: body_ids,
                },
            );
        }

        // 注册 verify 函数（如果有）
        if let Some(v) = verify {
            let body_ids: Vec<usize> = v.body.iter().map(|id| id.0).collect();
            skill_meta.insert(
                "verify".to_string(),
                Value::Task {
                    name: "verify".to_string(),
                    params: v.params.iter().map(|(n, _)| n.clone()).collect(),
                    v2_body_ids: body_ids,
                },
            );
        }

        // 存储 Skill Dict 到环境
        self.environment.lock().expect("env").define(
            name.to_string(),
            Value::Dict(skill_meta),
            false,
        );

        Ok(FlowSignal::None)
    }
}
