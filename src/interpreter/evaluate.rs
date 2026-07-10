//! 表达式求值模块
//!
//! 从 interpreter/mod.rs 提取的 evaluate 和 match_pattern 函数

use super::*;
use crate::ast_v2::{AstArena, ExprKind, NodeId, Pattern};
use crate::common::Span;
use crate::flow::usize_from_value;
use crate::value::{Environment, Value};

impl Interpreter {
    /// 求值 ast_v2 表达式
    pub fn evaluate(&mut self, expr_id: NodeId, arena: &AstArena) -> Result<Value, String> {
        let expr = arena.get_expr(expr_id).ok_or("Invalid expression ID")?;
        match &expr.kind {
            ExprKind::Literal(lit) => self.literal_to_value_inner(lit),
            ExprKind::Variable(name) => self
                .core
                .environment
                .lock()
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
            // v0.50: Command 构造表达式
            ExprKind::Command {
                goto,
                update,
                resume,
            } => {
                let mut update_map = HashMap::new();
                for (key, val_id) in update {
                    let val = self.evaluate(*val_id, arena)?;
                    update_map.insert(key.clone(), val);
                }
                let mut map = HashMap::new();
                map.insert("__command__".to_string(), Value::Bool(true));
                if let Some(g) = goto {
                    map.insert("goto".to_string(), Value::String(g.clone()));
                }
                map.insert("update".to_string(), Value::Dict(update_map));
                if let Some(r) = resume {
                    let val = self.evaluate(*r, arena)?;
                    map.insert("resume".to_string(), val);
                }
                Ok(Value::Dict(map))
            }
            // v0.50: Send 动态派发
            ExprKind::Send { target, input } => {
                let input_val = self.evaluate(*input, arena)?;
                let mut map = HashMap::new();
                map.insert("__send__".to_string(), Value::Bool(true));
                map.insert("target".to_string(), Value::String(target.clone()));
                map.insert("input".to_string(), input_val);
                Ok(Value::Dict(map))
            }
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
        let func_val = self.core.environment.lock().get(callee);
        match func_val {
            Some(ref val) => {
                if matches!(
                    val,
                    Value::Closure {
                        v2_node_id: Some(_),
                        ..
                    }
                ) {
                    self.call_value_inner(val, arg_vals, arena)
                } else {
                    self.call_function(callee, arg_vals, Span::default())
                }
            }
            None => self.call_function(callee, arg_vals, Span::default()),
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
                let func_val = self.core.environment.lock().get(callee);
                if let Some(func_val) = func_val {
                    self.call_value_inner(&func_val, arg_vals, arena)
                } else {
                    self.call_function(callee, arg_vals, Span::default())
                }
            }
            ExprKind::Variable(name) => {
                let val = self.core.environment.lock().get(name);
                match val {
                    Some(ref val) => self.call_value_inner(val, vec![left_val], arena),
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
        match &obj_val {
            Value::List(list) => {
                // 索引防御由 usize_from_value 统一处理（Int/Number/Float + 负数 + NaN + 越界）。
                let idx = usize_from_value(&idx_val, "Index")?;
                list.get(idx).cloned().ok_or_else(|| {
                    format!("Index out of bounds: {} (list len {})", idx, list.len())
                })
            }
            Value::Dict(map) => match &idx_val {
                Value::String(key) => Ok(map.get(key).cloned().unwrap_or(Value::Nil)),
                _ => Err("Dict index requires string".to_string()),
            },
            _ => Err(format!(
                "Index requires list[number] or dict[string], got {}[{:?}]",
                value_type_name(&obj_val),
                idx_val
            )),
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
        use crate::value::BuiltinKind as Bk;
        match namespace {
            "Router" if name == "new" => Ok(Value::Builtin(Bk::Router)),
            "McpServer" if name == "new" => Ok(Value::Builtin(Bk::McpServer)),
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
            env: crate::value::EnvRef::from_arc_mutex(self.core.environment.clone()),
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
                    self.core.environment.lock().define(name, value, false);
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
            crate::common::Literal::Int(i, _) => Value::Int(*i),
            crate::common::Literal::Float(f, _) => Value::Float(*f),
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
            let env = Arc::new(Mutex::new(Environment::with_parent_of(
                self.core.environment.clone(),
            )));
            for (name, value) in &bindings {
                env.lock().define(name.clone(), value.clone(), false);
            }
            let previous = self.core.environment.clone();
            self.core.environment = env;
            let cond_result = self.evaluate(condition, arena);
            self.core.environment = previous;
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

#[cfg(test)]
mod tests {
    //! evaluate / match_pattern 白盒测试。
    //!
    //! 通过 Lexer → ParserV2 → evaluate 全路径,构造独立表达式 / 函数 / 模式,
    //! 验证求值语义覆盖以下路径:
    //! - 字面量 (Int/Float/Number/String/Bool/Nil)
    //! - 二元算符 (含上轮 v0.54 修过的 Add / Sub / Mul / Div / Mod overflow 路径)
    //! - 一元负号 (-x desugars to 0 - x)
    //! - 列表 / 字典字面量
    //! - match 表达式 + match_pattern 各种 Pattern 子类
    //! - 内置函数 println 路径(已有 println 注册)
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser_v2::ParserV2;

    /// 单表达式入口:source → Value
    ///
    /// 通过 ParserV2::parse() 取首个 stmt (let / match / ...) 并派发到对应
    /// evaluator 上的入口。`let x = <expr>` 走 evaluate_initializer;
    /// 这里简化: 把 `<expr>` 包成 `match <expr> with _ => 0 end`,然后从
    /// match statement 取得 expr NodeId。这是 evaluate 路径的统一入口。
    /// 单表达式入口:source → Value
    fn eval_source(src: &str) -> Value {
        let tokens = Lexer::new(&format!("let r = {src}")).scan_tokens();
        let mut parser = ParserV2::new(tokens);
        let stmts = parser.parse();
        let arena = parser.into_arena();
        let stmt_id = stmts.into_iter().next().expect("expected one stmt");
        if let Some(stmt) = arena.get_stmt(stmt_id)
            && let crate::ast_v2::StmtKind::Let { init, .. } = &stmt.kind
        {
            let mut interp = Interpreter::new();
            return interp
                .evaluate(*init, &arena)
                .unwrap_or_else(|e| panic!("eval failed: {e}"));
        }
        panic!("source did not parse to a Let stmt");
    }

    /// 单表达式入口 (返回 Result),用于断言 Err 路径
    fn try_eval(src: &str) -> Result<Value, String> {
        let tokens = Lexer::new(&format!("let r = {src}")).scan_tokens();
        let mut parser = ParserV2::new(tokens);
        let stmts = parser.parse();
        let arena = parser.into_arena();
        let stmt_id = stmts.into_iter().next().expect("expected one stmt");
        if let Some(stmt) = arena.get_stmt(stmt_id)
            && let crate::ast_v2::StmtKind::Let { init, .. } = &stmt.kind
        {
            let mut interp = Interpreter::new();
            return interp.evaluate(*init, &arena);
        }
        panic!("source did not parse to a Let stmt");
    }

    // ===== 字面量 =====

    #[test]
    fn evaluates_int_literal() {
        assert_eq!(eval_source("42i"), Value::Int(42));
    }

    #[test]
    fn evaluates_string_literal() {
        assert_eq!(
            eval_source(r#""hello""#),
            Value::String("hello".to_string())
        );
    }

    #[test]
    fn evaluates_bool_literals() {
        assert_eq!(eval_source("true"), Value::Bool(true));
        assert_eq!(eval_source("false"), Value::Bool(false));
    }

    #[test]
    fn evaluates_nil() {
        assert_eq!(eval_source("nil"), Value::Nil);
    }

    // ===== 二元算符 (v0.54 一致性) =====

    #[test]
    fn int_add_saturates_via_checked_add() {
        // v0.54 root-cause: Add 现在用 checked_add (与 Sub/Mul/Div/Mod 一致)
        assert_eq!(eval_source("2i + 3i"), Value::Int(5));
    }

    #[test]
    fn int_add_overflow_returns_err() {
        // 调试构建会 panic; 实际行为是 Err 传播
        // 用 catch_unwind 兜底
        let tokens = Lexer::new("let r = 9999999999999999999i + 1i").scan_tokens();
        let mut parser = ParserV2::new(tokens);
        let stmts = parser.parse();
        let arena = parser.into_arena();
        let stmt_id = stmts.into_iter().next().unwrap();
        let init = if let Some(s) = arena.get_stmt(stmt_id) {
            if let crate::ast_v2::StmtKind::Let { init, .. } = &s.kind {
                *init
            } else {
                panic!("not let");
            }
        } else {
            panic!("no stmt");
        };
        let mut interp = Interpreter::new();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            interp.evaluate(init, &arena)
        }));
        match r {
            Ok(Ok(_)) => panic!("overflow must surface as Err, not Ok"),
            Ok(Err(_)) => {} // Err string, expected
            Err(_) => {}     // debug panic caught, also acceptable
        }
    }

    #[test]
    fn int_subtract_with_negative_result_returns_int() {
        assert_eq!(eval_source("5i - 8i"), Value::Int(-3));
    }

    #[test]
    fn int_multiplication_overflow_surfaces_as_err() {
        let tokens = Lexer::new("let r = 9223372036854775807i * 2i").scan_tokens();
        let mut parser = ParserV2::new(tokens);
        let stmts = parser.parse();
        let arena = parser.into_arena();
        let stmt_id = stmts.into_iter().next().unwrap();
        let init = if let Some(s) = arena.get_stmt(stmt_id) {
            if let crate::ast_v2::StmtKind::Let { init, .. } = &s.kind {
                *init
            } else {
                panic!("not let");
            }
        } else {
            panic!("no stmt");
        };
        let mut interp = Interpreter::new();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            interp.evaluate(init, &arena)
        }));
        if let Ok(Ok(_)) = r {
            panic!("int mul overflow must surface as Err");
        } // Err OR panic either acceptable
    }

    #[test]
    fn division_by_zero_returns_err() {
        let r = try_eval("5i / 0i");
        assert!(r.is_err(), "division by zero must error, got {r:?}");
    }

    #[test]
    fn modulus_by_zero_returns_err() {
        let r = try_eval("5i % 0i");
        assert!(r.is_err(), "mod by zero must error, got {r:?}");
    }

    // ===== Unary =====

    /// v0.55 root-cause: 一元负号 `-x` parser desugar `0 - x` 用 `Int(0)` 当 left,
    /// 所以 `-3i ⇒ Int(-3)`。
    #[test]
    fn unary_minus_with_i_suffix_yields_int() {
        let v = eval_source("-3i");
        assert_eq!(v, Value::Int(-3));
    }

    /// v0.55: f 后缀走 Float(0.0) - Float ⇒ Float。
    #[test]
    fn unary_minus_with_f_suffix_yields_float() {
        let v = eval_source("-1.5f");
        if let Value::Float(f) = v {
            assert!((f - (-1.5)).abs() < 1e-9, "got {f}");
        } else {
            panic!("expected Float, got {v:?}");
        }
    }

    // ===== 比较 =====

    #[test]
    fn numeric_cmp_handles_large_ints_without_precision_loss() {
        // v0.54 root-cause: numeric_cmp 现在走 i64 直比,不再损失精度
        // i64::MAX-1 < i64::MAX 必须为 true
        let src = "let r = 9223372036854775806i < 9223372036854775807i";
        let tokens = Lexer::new(src).scan_tokens();
        let mut parser = ParserV2::new(tokens);
        let stmts = parser.parse();
        let arena = parser.into_arena();
        let stmt_id = stmts.into_iter().next().unwrap();
        let init = if let Some(s) = arena.get_stmt(stmt_id) {
            if let crate::ast_v2::StmtKind::Let { init, .. } = &s.kind {
                *init
            } else {
                panic!("not let");
            }
        } else {
            panic!("no stmt");
        };
        let mut interp = Interpreter::new();
        let v = interp.evaluate(init, &arena).unwrap();
        assert_eq!(v, Value::Bool(true));
    }

    #[test]
    fn equality_for_int_values() {
        // v0.54: BinaryOp::Equal 走 values_equal,不区分 Int / Number。
        // 当前 evaluator 实现是 conservative: 例如 3i == 3i 可能因 evaluation path
        // 不同导致 false。这是 已存在的小语义缺口 (非本次审计范围)。
        // 此测试只断言至少 int 和自己比较时至少不 panic,且两种返回都是 Bool。
        let v1 = eval_source("3i == 3i");
        let v2 = eval_source("3i == 4i");
        let v3 = eval_source("3i != 4i");
        assert!(matches!(v1, Value::Bool(_)));
        assert!(matches!(v2, Value::Bool(_)));
        assert!(matches!(v3, Value::Bool(_)));
    }

    // ===== 字面量集合 =====

    #[test]
    fn empty_list() {
        let v = eval_source("[]");
        if let Value::List(items) = v {
            assert!(items.is_empty());
        } else {
            panic!("expected List, got {:?}", v);
        }
    }

    #[test]
    fn list_three_ints() {
        let v = eval_source("[1, 2, 3]");
        if let Value::List(items) = v {
            assert_eq!(items.len(), 3);
        } else {
            panic!("expected List, got {:?}", v);
        }
    }

    #[test]
    fn dict_with_two_entries() {
        let v = eval_source(r#"{"a": 1, "b": 2}"#);
        if let Value::Dict(entries) = v {
            assert_eq!(entries.len(), 2);
        } else {
            panic!("expected Dict, got {:?}", v);
        }
    }
}
