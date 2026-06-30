//! 表达式求值模块
//!
//! 从 interpreter/mod.rs 提取的 evaluate 和 match_pattern 函数

use super::*;
use crate::ast_v2::{AstArena, ExprKind, NodeId, Pattern};
use crate::common::Span;
use crate::value::{Environment, Value};

impl Interpreter {
    /// 求值 ast_v2 表达式
    pub fn evaluate(&mut self, expr_id: NodeId, arena: &AstArena) -> Result<Value, String> {
        let expr = arena.get_expr(expr_id).ok_or("Invalid expression ID")?;
        match &expr.kind {
            ExprKind::Literal(lit) => self.literal_to_value_inner(lit),
            ExprKind::Variable(name) => self
                .environment
                .lock()
                .expect("environment mutex poisoned")
                .get(name)
                .ok_or_else(|| format!("Undefined variable: {}", name)),
            ExprKind::Binary { left, op, right } => {
                let left_val = self.evaluate(*left, arena)?;
                let right_val = self.evaluate(*right, arena)?;
                eval_binary(left_val, op, right_val)
            }
            ExprKind::Call { callee, args } => self.evaluate_call(callee, args, arena),
            ExprKind::Grouping(inner) => self.evaluate(*inner, arena),
            ExprKind::Pipe { left, right } => self.evaluate_pipe(*left, *right, arena),
            ExprKind::MethodCall {
                object,
                method,
                args,
            } => self.evaluate_method_call(*object, method, args, arena),
            ExprKind::Index { object, index } => self.evaluate_index(*object, *index, arena),
            ExprKind::Question { expr } => self.evaluate_question(*expr, arena),
            ExprKind::NamespaceRef { namespace, name } => {
                self.evaluate_namespace_ref(namespace, name)
            }
            ExprKind::Closure {
                params, body: _, ..
            } => self.evaluate_closure(expr_id, params),
            ExprKind::Borrow { expr: inner } | ExprKind::BorrowMut { expr: inner } => {
                self.evaluate_borrow(*inner, arena)
            }
            ExprKind::Prompt { parts } => self.evaluate_prompt(parts, arena),
            ExprKind::List(items) => self.evaluate_list(items, arena),
            ExprKind::Dict(entries) => self.evaluate_dict(entries, arena),
            ExprKind::Match { expr, arms } => self.evaluate_match_expr(*expr, arms, arena),
            _ => Err(format!("Unsupported v2 expression: {:?}", expr.kind)),
        }
    }

    /// 求值函数调用
    fn evaluate_call(
        &mut self,
        callee: &str,
        args: &[NodeId],
        arena: &AstArena,
    ) -> Result<Value, String> {
        let mut arg_vals = Vec::new();
        for arg_id in args {
            arg_vals.push(self.evaluate(*arg_id, arena)?);
        }
        // v2 路径: 先从环境查找（注意 environment 和 globals 可能是同一个 Arc，不能双锁）
        let func_val = self.environment.lock().expect("env").get(callee);
        match func_val {
            Some(Value::Closure {
                v2_node_id: Some(_),
                ..
            }) => self.call_value_inner(&func_val.unwrap(), arg_vals, arena),
            _ => self.call_function(callee, arg_vals, Span::default()),
        }
    }

    /// 求值管道表达式
    fn evaluate_pipe(
        &mut self,
        left: NodeId,
        right: NodeId,
        arena: &AstArena,
    ) -> Result<Value, String> {
        let left_val = self.evaluate(left, arena)?;
        let right_expr = arena.get_expr(right).ok_or("Invalid pipe right")?;
        match &right_expr.kind {
            ExprKind::MethodCall {
                object: _,
                method,
                args,
            } => {
                let mut arg_vals = Vec::new();
                for arg_id in args {
                    arg_vals.push(self.evaluate(*arg_id, arena)?);
                }
                self.call_method(left_val, method, arg_vals, Span::default())
            }
            ExprKind::Call { callee, args } => {
                let mut arg_vals = vec![left_val];
                for arg_id in args {
                    arg_vals.push(self.evaluate(*arg_id, arena)?);
                }
                let func_val = self.environment.lock().expect("env").get(callee);
                if let Some(func_val) = func_val {
                    self.call_value_inner(&func_val, arg_vals, arena)
                } else {
                    self.call_function(callee, arg_vals, Span::default())
                }
            }
            ExprKind::Variable(name) => {
                let val = self.environment.lock().expect("env").get(name);
                match val {
                    Some(Value::Closure {
                        v2_node_id: Some(_),
                        ..
                    }) => self.call_value_inner(&val.unwrap(), vec![left_val], arena),
                    Some(_) => self.call_value_inner(&val.unwrap(), vec![left_val], arena),
                    None => self.call_method(left_val, name, vec![], Span::default()),
                }
            }
            _ => Err(format!("Unsupported pipe right: {:?}", right_expr.kind)),
        }
    }

    /// 求值方法调用
    fn evaluate_method_call(
        &mut self,
        object: NodeId,
        method: &str,
        args: &[NodeId],
        arena: &AstArena,
    ) -> Result<Value, String> {
        let obj_val = self.evaluate(object, arena)?;
        let mut arg_vals = Vec::new();
        for arg_id in args {
            arg_vals.push(self.evaluate(*arg_id, arena)?);
        }
        self.call_method(obj_val, method, arg_vals, Span::default())
    }

    /// 求值索引访问
    fn evaluate_index(
        &mut self,
        object: NodeId,
        index: NodeId,
        arena: &AstArena,
    ) -> Result<Value, String> {
        let obj_val = self.evaluate(object, arena)?;
        let idx_val = self.evaluate(index, arena)?;
        match (&obj_val, &idx_val) {
            (Value::List(list), Value::Number(n)) => {
                let idx = *n as usize;
                if idx < list.len() {
                    Ok(list[idx].clone())
                } else {
                    Err(format!(
                        "Index out of bounds: {} (list len {})",
                        idx,
                        list.len()
                    ))
                }
            }
            (Value::Dict(map), Value::String(key)) => {
                Ok(map.get(key).cloned().unwrap_or(Value::Nil))
            }
            _ => Err("Index requires list[number] or dict[string]".to_string()),
        }
    }

    /// 求值错误传播
    fn evaluate_question(&mut self, expr: NodeId, arena: &AstArena) -> Result<Value, String> {
        let val = self.evaluate(expr, arena)?;
        match val {
            Value::Dict(ref map) if map.contains_key("err") => {
                Err(format!("Error propagated: {:?}", map.get("err")))
            }
            _ => Ok(val),
        }
    }

    /// 求值命名空间引用
    fn evaluate_namespace_ref(&self, namespace: &str, name: &str) -> Result<Value, String> {
        match namespace {
            "Router" if name == "new" => Ok(Value::Builtin("Router::new".to_string())),
            "McpServer" if name == "new" => Ok(Value::Builtin("McpServer::new".to_string())),
            _ => Err(format!("Unknown namespace ref: {}::{}", namespace, name)),
        }
    }

    /// 求值闭包
    fn evaluate_closure(
        &self,
        expr_id: NodeId,
        params: &[(String, Option<String>)],
    ) -> Result<Value, String> {
        let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
        Ok(Value::Closure {
            params: param_names,
            env: self.environment.clone(),
            v2_node_id: Some(expr_id.0),
        })
    }

    /// 求值借用
    fn evaluate_borrow(&mut self, inner: NodeId, arena: &AstArena) -> Result<Value, String> {
        let val = self.evaluate(inner, arena)?;
        Ok(Value::Atom(Arc::new(Mutex::new(val))))
    }

    /// 求值模板字符串
    fn evaluate_prompt(&mut self, parts: &[NodeId], arena: &AstArena) -> Result<Value, String> {
        let mut result = String::new();
        for part_id in parts {
            let val = self.evaluate(*part_id, arena)?;
            result.push_str(&val.to_string());
        }
        Ok(Value::String(result))
    }

    /// 求值列表字面量
    fn evaluate_list(&mut self, items: &[NodeId], arena: &AstArena) -> Result<Value, String> {
        let mut values = Vec::new();
        for item_id in items {
            values.push(self.evaluate(*item_id, arena)?);
        }
        Ok(Value::List(values))
    }

    /// 求值字典字面量
    fn evaluate_dict(
        &mut self,
        entries: &[(String, NodeId)],
        arena: &AstArena,
    ) -> Result<Value, String> {
        let mut map = HashMap::new();
        for (key, val_id) in entries {
            map.insert(key.clone(), self.evaluate(*val_id, arena)?);
        }
        Ok(Value::Dict(map))
    }

    /// 求值模式匹配表达式
    fn evaluate_match_expr(
        &mut self,
        expr: NodeId,
        arms: &[(Pattern, NodeId)],
        arena: &AstArena,
    ) -> Result<Value, String> {
        let val = self.evaluate(expr, arena)?;
        for (pattern, result_id) in arms {
            if let Some(bindings) = self.match_pattern(pattern, &val, arena) {
                // 绑定模式变量
                for (name, value) in bindings {
                    self.environment
                        .lock()
                        .expect("env")
                        .define(name, value, false);
                }
                return self.evaluate(*result_id, arena);
            }
        }
        Ok(Value::Nil)
    }

    /// 模式匹配
    pub(super) fn match_pattern(
        &mut self,
        pattern: &Pattern,
        value: &Value,
        arena: &AstArena,
    ) -> Option<Vec<(String, Value)>> {
        match (pattern, value) {
            (Pattern::Wildcard, _) => Some(vec![]),
            (Pattern::Variable(name), _) => Some(vec![(name.clone(), value.clone())]),
            (Pattern::Literal(lit), val) => self.match_literal_pattern(lit, val),
            (Pattern::List { prefix, rest }, Value::List(vals)) => {
                self.match_list_pattern(prefix, rest.as_deref(), vals, arena)
            }
            (Pattern::Dict(pats), Value::Dict(map)) => self.match_dict_pattern(pats, map, arena),
            (Pattern::Guard { pattern, condition }, val) => {
                self.match_guard_pattern(pattern, *condition, val, arena)
            }
            _ => None,
        }
    }

    /// 匹配字面量模式
    fn match_literal_pattern(
        &self,
        lit: &crate::common::Literal,
        val: &Value,
    ) -> Option<Vec<(String, Value)>> {
        let lit_val = match lit {
            crate::common::Literal::String(s, _) => Value::String(s.clone()),
            crate::common::Literal::Char(c, _) => Value::Char(*c),
            crate::common::Literal::Number(n, _) => Value::Number(*n),
            crate::common::Literal::Bool(b, _) => Value::Bool(*b),
            crate::common::Literal::Nil(_) => Value::Nil,
        };
        if values_equal(&lit_val, val) {
            Some(vec![])
        } else {
            None
        }
    }

    /// 匹配列表模式
    fn match_list_pattern(
        &mut self,
        prefix: &[Pattern],
        rest: Option<&str>,
        vals: &[Value],
        arena: &AstArena,
    ) -> Option<Vec<(String, Value)>> {
        if let Some(rest_name) = rest {
            if vals.len() < prefix.len() {
                return None;
            }
            let mut bindings = Vec::new();
            for (pat, val) in prefix.iter().zip(vals.iter()) {
                if let Some(b) = self.match_pattern(pat, val, arena) {
                    bindings.extend(b);
                } else {
                    return None;
                }
            }
            let rest_vals: Vec<Value> = vals[prefix.len()..].to_vec();
            bindings.push((rest_name.to_string(), Value::List(rest_vals)));
            Some(bindings)
        } else {
            if prefix.len() != vals.len() {
                return None;
            }
            let mut bindings = Vec::new();
            for (pat, val) in prefix.iter().zip(vals.iter()) {
                if let Some(b) = self.match_pattern(pat, val, arena) {
                    bindings.extend(b);
                } else {
                    return None;
                }
            }
            Some(bindings)
        }
    }

    /// 匹配字典模式
    fn match_dict_pattern(
        &mut self,
        pats: &[(String, Pattern)],
        map: &HashMap<String, Value>,
        arena: &AstArena,
    ) -> Option<Vec<(String, Value)>> {
        let mut bindings = Vec::new();
        for (key, pat) in pats.iter() {
            if let Some(val) = map.get(key) {
                if let Some(b) = self.match_pattern(pat, val, arena) {
                    bindings.extend(b);
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
        Some(bindings)
    }

    /// 匹配守卫模式
    fn match_guard_pattern(
        &mut self,
        pattern: &Pattern,
        condition: NodeId,
        val: &Value,
        arena: &AstArena,
    ) -> Option<Vec<(String, Value)>> {
        if let Some(bindings) = self.match_pattern(pattern, val, arena) {
            let env = Arc::new(Mutex::new(Environment::with_parent(
                self.environment.clone(),
            )));
            for (name, value) in &bindings {
                env.lock()
                    .expect("env")
                    .define(name.clone(), value.clone(), false);
            }
            let previous = self.environment.clone();
            self.environment = env;
            let cond_result = self.evaluate(condition, arena);
            self.environment = previous;
            match cond_result {
                Ok(Value::Bool(true)) => Some(bindings),
                Ok(Value::Bool(false)) | Ok(_) => None,
                Err(_) => None,
            }
        } else {
            None
        }
    }
}
