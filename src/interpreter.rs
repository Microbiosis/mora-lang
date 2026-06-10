use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::time::Duration;

use crate::ast::*;
use crate::lexer::Lexer;
use crate::parser::Parser;

// v10 HTTP 超时配置
const HTTP_READ_TIMEOUT_SECS: u64 = 30;
const HTTP_WRITE_TIMEOUT_SECS: u64 = 10;
const AI_READ_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Clone)]
pub enum Value {
    String(String),
    Number(f64),
    Bool(bool),
    Nil,
    List(Vec<Value>),
    Dict(HashMap<String, Value>),
    Task {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    Closure {
        params: Vec<String>,
        body: Vec<Stmt>,
        env: Arc<Mutex<Environment>>,
    },
    Builtin(String),
    // v10: 多轮对话对象
    Conversation {
        messages: Vec<(String, String)>, // (role, content) 历史
        model: String,
        base_url: String,
        api_key: String,
    },
}

// 手动实现 PartialEq（Arc<Mutex<Environment>> 不支持自动派生）
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Dict(a), Value::Dict(b)) => a == b,
            _ => false,
        }
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::String(s) => write!(f, "{}", s),
            Value::Number(n) => write!(f, "{}", n),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Nil => write!(f, "nil"),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.to_string()).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            Value::Dict(map) => {
                let parts: Vec<String> = map.iter()
                    .map(|(k, v)| format!("{}: {}", k, v))
                    .collect();
                write!(f, "{{{}}}", parts.join(", "))
            }
            Value::Task { name, .. } => write!(f, "<task {}>", name),
            Value::Closure { .. } => write!(f, "<closure>"),
            Value::Builtin(name) => write!(f, "<builtin {}>", name),
            Value::Conversation { model, messages, .. } => {
                write!(f, "<conversation {} ({} messages)>", model, messages.len())
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct Environment {
    values: HashMap<String, Value>,
    exports: HashMap<String, Value>,
    parent: Option<Arc<Mutex<Environment>>>,
}

impl Environment {
    fn new() -> Self {
        Self { values: HashMap::new(), exports: HashMap::new(), parent: None }
    }

    fn with_parent(parent: Arc<Mutex<Environment>>) -> Self {
        Self { values: HashMap::new(), exports: HashMap::new(), parent: Some(parent) }
    }

    fn define(&mut self, name: String, value: Value, exported: bool) {
        self.values.insert(name.clone(), value.clone());
        if exported {
            self.exports.insert(name, value);
        }
    }

    fn get(&self, name: &str) -> Option<Value> {
        if let Some(value) = self.values.get(name) {
            Some(value.clone())
        } else if let Some(parent) = &self.parent {
            parent.lock().unwrap().get(name)
        } else {
            None
        }
    }

    fn assign(&mut self, name: &str, value: Value) -> bool {
        if self.values.contains_key(name) {
            self.values.insert(name.to_string(), value);
            true
        } else if let Some(parent) = &self.parent {
            parent.lock().unwrap().assign(name, value)
        } else {
            false
        }
    }
}

pub struct Interpreter {
    globals: Arc<Mutex<Environment>>,
    environment: Arc<Mutex<Environment>>,
}

impl Interpreter {
    pub fn new() -> Self {
        let globals = Arc::new(Mutex::new(Environment::new()));
        globals.lock().unwrap().define("print".to_string(), Value::Builtin("print".to_string()), false);
        globals.lock().unwrap().define("range".to_string(), Value::Builtin("range".to_string()), false);
        globals.lock().unwrap().define("len".to_string(), Value::Builtin("len".to_string()), false);
        Self { globals: globals.clone(), environment: globals }
    }

    pub fn new_with_globals(globals: Arc<Mutex<Environment>>) -> Self {
        let env = Arc::new(Mutex::new(Environment::with_parent(globals.clone())));
        Self { globals: globals.clone(), environment: env }
    }

    #[allow(dead_code)]
    pub fn get_globals(&self) -> Arc<Mutex<Environment>> {
        self.globals.clone()
    }

    pub fn interpret(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        for stmt in stmts {
            self.execute(stmt)?;
        }
        // 先 clone 出值，再释放 borrow，避免借用冲突
        let main_task = self.globals.lock().unwrap().get("main").clone();
        if let Some(Value::Task { params, body, .. }) = main_task {
            if params.is_empty() {
                let params = params.clone();
                let body = body.clone();
                self.call_task(&params, &body, vec![])?;
            }
        }
        Ok(())
    }

    pub fn execute(&mut self, stmt: &Stmt) -> Result<Option<Value>, String> {
        match stmt {
            Stmt::Let { name, type_hint, init, exported } => {
                let value = self.evaluate(init)?;
                if let Some(hint) = type_hint {
                    if !check_type(&value, hint) {
                        return Err(format!("Type mismatch: expected {}, got {}", hint, type_name(&value)));
                    }
                }
                self.environment.lock().unwrap().define(name.clone(), value, *exported);
                Ok(None)
            }
            Stmt::Assign { name, value } => {
                let val = self.evaluate(value)?;
                if !self.environment.lock().unwrap().assign(name, val.clone()) {
                    self.environment.lock().unwrap().define(name.clone(), val, false);
                }
                Ok(None)
            }
            Stmt::IndexAssign { object, index, value } => {
                let obj = self.evaluate(object)?;
                let idx = self.evaluate(index)?;
                let val = self.evaluate(value)?;
                match (&obj, &idx) {
                    (Value::List(list), Value::Number(n)) => {
                        let i = *n as usize;
                        if i < list.len() {
                            let mut new_list = list.clone();
                            new_list[i] = val;
                            Ok(None)
                        } else {
                            Err(format!("Index out of bounds: {} (len: {})", i, list.len()))
                        }
                    }
                    _ => Err("Can only index assign to lists".to_string()),
                }
            }
            Stmt::TaskDef { name, params, body, exported } => {
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                let task = Value::Task { name: name.clone(), params: param_names, body: body.clone() };
                self.environment.lock().unwrap().define(name.clone(), task, *exported);
                Ok(None)
            }
            Stmt::If { condition, then_branch } => {
                let cond = self.evaluate(condition)?;
                if is_truthy(&cond) {
                    let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                    self.execute_block(then_branch, env)
                } else {
                    Ok(None)
                }
            }
            Stmt::For { var, iterable, body } => {
                let iter_val = self.evaluate(iterable)?;
                match iter_val {
                    Value::List(items) => {
                        for item in items {
                            let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                            env.lock().unwrap().define(var.clone(), item, false);
                            let result = self.execute_block(body, env)?;
                            if result.is_some() { return Ok(result); }
                        }
                        Ok(None)
                    }
                    Value::String(s) => {
                        for ch in s.chars() {
                            let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                            env.lock().unwrap().define(var.clone(), Value::String(ch.to_string()), false);
                            let result = self.execute_block(body, env)?;
                            if result.is_some() { return Ok(result); }
                        }
                        Ok(None)
                    }
                    _ => Err(format!("Cannot iterate over {}", iter_val)),
                }
            }
            Stmt::Try { try_block, catch_var, catch_block } => {
                let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                match self.execute_block(try_block, env.clone()) {
                    Ok(result) => Ok(result),
                    Err(err_msg) => {
                        env.lock().unwrap().define(catch_var.clone(), Value::String(err_msg), false);
                        self.execute_block(catch_block, env)
                    }
                }
            }
            Stmt::Import { path } => {
                let module_env = self.import_module(path)?;
                let exports = module_env.lock().unwrap().exports.clone();
                for (name, value) in exports {
                    self.environment.lock().unwrap().define(name, value, false);
                }
                Ok(None)
            }
            Stmt::Parallel { stmts } => {
                self.execute_parallel(stmts)
            }
            Stmt::Match { expr, arms } => {
                let val = self.evaluate(expr)?;
                for (pattern, arm_stmts) in arms {
                    if let Some(bindings) = self.match_pattern(pattern, &val) {
                        let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                        for (name, value) in bindings {
                            env.lock().unwrap().define(name, value, false);
                        }
                        return self.execute_block(arm_stmts, env);
                    }
                }
                Err("No match arm matched".to_string())
            }
            Stmt::Save { path, value } => {
                let path_val = self.evaluate(path)?;
                let data_val = self.evaluate(value)?;
                let path_str = match path_val {
                    Value::String(s) => s,
                    _ => return Err("save path must be a string".to_string()),
                };
                let json = value_to_json(&data_val);
                fs::write(&path_str, json).map_err(|e| format!("Failed to save: {}", e))?;
                println!("[save] {} -> {}", path_str, type_name(&data_val));
                Ok(None)
            }
            Stmt::Load { path, var } => {
                let path_val = self.evaluate(path)?;
                let path_str = match path_val {
                    Value::String(s) => s,
                    _ => return Err("load path must be a string".to_string()),
                };
                let json = fs::read_to_string(&path_str).map_err(|e| format!("Failed to load: {}", e))?;
                let value = json_to_value(&json)?;
                self.environment.lock().unwrap().define(var.clone(), value, false);
                println!("[load] {} -> {}", path_str, var);
                Ok(None)
            }
            Stmt::Return { value } => {
                let val = match value {
                    Some(expr) => self.evaluate(expr)?,
                    None => Value::Nil,
                };
                Ok(Some(val))
            }
            Stmt::Expr(expr) => {
                let val = self.evaluate(expr)?;
                Ok(Some(val))
            }
        }
    }

    fn execute_block(&mut self, stmts: &[Stmt], env: Arc<Mutex<Environment>>) -> Result<Option<Value>, String> {
        let previous = self.environment.clone();
        self.environment = env;
        let mut result = None;
        for stmt in stmts {
            result = self.execute(stmt)?;
            // 只有显式 Return 语句才中断块执行
            if matches!(stmt, Stmt::Return { .. }) && result.is_some() {
                break;
            }
        }
        self.environment = previous;
        Ok(result)
    }

    fn execute_parallel(&mut self, stmts: &[Stmt]) -> Result<Option<Value>, String> {
        // Arc<Mutex> 替代 Rc<RefCell> 后，Value 实现了 Send，
        // 可以在 scoped threads 中返回。
        let globals = self.globals.clone();
        let mut values = Vec::new();

        std::thread::scope(|s| {
            let mut handles = Vec::new();
            for stmt in stmts {
                let globals = globals.clone();
                let stmt = stmt.clone();
                handles.push(s.spawn(move || {
                    let mut interpreter = Interpreter::new_with_globals(globals);
                    interpreter.execute(&stmt)
                }));
            }
            for handle in handles {
                match handle.join() {
                    Ok(Ok(Some(v))) => values.push(v),
                    Ok(Ok(None)) => values.push(Value::Nil),
                    Ok(Err(e)) => {
                        eprintln!("Parallel task error: {}", e);
                        values.push(Value::Nil);
                    }
                    Err(_) => {
                        eprintln!("Parallel task panicked");
                        values.push(Value::Nil);
                    }
                }
            }
        });

        Ok(Some(Value::List(values)))
    }

    fn match_pattern(&self, pattern: &Pattern, value: &Value) -> Option<Vec<(String, Value)>> {
        match (pattern, value) {
            (Pattern::Wildcard, _) => Some(vec![]),
            (Pattern::Variable(name), _) => Some(vec![(name.clone(), value.clone())]),
            (Pattern::Literal(lit), val) => {
                let lit_val = literal_to_value_static(lit);
                if values_equal(&lit_val, val) {
                    Some(vec![])
                } else {
                    None
                }
            }
            (Pattern::List(pats), Value::List(vals)) => {
                if pats.len() != vals.len() {
                    return None;
                }
                let mut bindings = Vec::new();
                for (pat, val) in pats.iter().zip(vals.iter()) {
                    if let Some(b) = self.match_pattern(pat, val) {
                        bindings.extend(b);
                    } else {
                        return None;
                    }
                }
                Some(bindings)
            }
            (Pattern::Dict(pats), Value::Dict(map)) => {
                let mut bindings = Vec::new();
                for (key, pat) in pats.iter() {
                    if let Some(val) = map.get(key) {
                        if let Some(b) = self.match_pattern(pat, val) {
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
            _ => None,
        }
    }

    fn import_module(&mut self, path: &str) -> Result<Arc<Mutex<Environment>>, String> {
        let file_path = format!("{}.mora", path);
        let source = fs::read_to_string(&file_path)
            .map_err(|e| format!("Failed to load module '{}': {}", path, e))?;

        let mut lexer = Lexer::new(&source);
        let tokens = lexer.scan_tokens();
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse();

        let module_env = Arc::new(Mutex::new(Environment::with_parent(self.globals.clone())));
        let previous = self.environment.clone();
        self.environment = module_env.clone();

        for stmt in &stmts {
            self.execute(stmt)?;
        }

        self.environment = previous;
        Ok(module_env)
    }

    fn evaluate(&mut self, expr: &Expr) -> Result<Value, String> {
        match expr {
            Expr::Literal(lit) => self.literal_to_value(lit),
            Expr::Variable(name) => {
                let value = self.environment.lock().unwrap().get(name);
                match value {
                    Some(v) => Ok(v),
                    None if is_builtin_object(name) => Ok(Value::Builtin(name.clone())),
                    None => Err(format!("Undefined variable: {}", name)),
                }
            }
            Expr::Grouping(expr) => self.evaluate(expr),
            Expr::Binary { left, op, right } => {
                let left = self.evaluate(left)?;
                let right = self.evaluate(right)?;
                eval_binary(left, op, right)
            }
            Expr::Pipe { left, right } => {
                let left_val = self.evaluate(left)?;
                self.evaluate_pipe(left_val, right)
            }
            Expr::Call { callee, args } => {
                let arg_values: Result<Vec<Value>, String> = args.iter().map(|a| self.evaluate(a)).collect();
                self.call_function(callee, arg_values?)
            }
            Expr::MethodCall { object, method, args } => {
                let obj = self.evaluate(object)?;
                let arg_values: Result<Vec<Value>, String> = args.iter().map(|a| self.evaluate(a)).collect();
                self.call_method(obj, method, arg_values?)
            }
            Expr::Index { object, index } => {
                let obj = self.evaluate(object)?;
                let idx = self.evaluate(index)?;
                match (&obj, &idx) {
                    (Value::List(list), Value::Number(n)) => {
                        let i = *n as usize;
                        if i < list.len() { Ok(list[i].clone()) }
                        else { Err(format!("Index out of bounds: {} (len: {})", i, list.len())) }
                    }
                    (Value::String(s), Value::Number(n)) => {
                        let i = *n as usize;
                        if i < s.len() { Ok(Value::String(s.chars().nth(i).unwrap().to_string())) }
                        else { Err(format!("Index out of bounds: {} (len: {})", i, s.len())) }
                    }
                    (Value::Dict(map), Value::String(key)) => {
                        Ok(map.get(key).cloned().unwrap_or(Value::Nil))
                    }
                    _ => Err(format!("Cannot index {} with {}", obj, idx)),
                }
            }
            Expr::Closure { params, body } => {
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                Ok(Value::Closure {
                    params: param_names,
                    body: body.clone(),
                    env: self.environment.clone(),
                })
            }
            Expr::Match { expr, arms } => {
                let val = self.evaluate(expr)?;
                for (pattern, arm_expr) in arms.iter() {
                    if let Some(bindings) = self.match_pattern(pattern, &val) {
                        let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                        for (name, value) in bindings {
                            env.lock().unwrap().define(name, value, false);
                        }
                        let previous = self.environment.clone();
                        self.environment = env;
                        let result = self.evaluate(arm_expr);
                        self.environment = previous;
                        return result;
                    }
                }
                Err("No match arm matched".to_string())
            }
        }
    }

    fn evaluate_pipe(&mut self, left_val: Value, right: &Expr) -> Result<Value, String> {
        match right {
            Expr::Call { callee, args } => {
                // 检查是否是列表/字符串方法名——自动转为方法调用
                if is_pipe_method(callee) {
                    let mut arg_values: Vec<Value> = Vec::new();
                    for arg in args {
                        arg_values.push(self.evaluate(arg)?);
                    }
                    return self.call_method(left_val, callee, arg_values);
                }
                let mut arg_values: Vec<Value> = vec![left_val];
                for arg in args {
                    arg_values.push(self.evaluate(arg)?);
                }
                self.call_function(callee, arg_values)
            }
            Expr::MethodCall { object, method, args } => {
                let obj = self.evaluate(object)?;
                let mut arg_values: Vec<Value> = vec![left_val];
                for arg in args {
                    arg_values.push(self.evaluate(arg)?);
                }
                self.call_method(obj, method, arg_values)
            }
            Expr::Variable(name) => {
                self.call_function(name, vec![left_val])
            }
            Expr::Pipe { left: inner_left, right: inner_right } => {
                let inner_val = self.evaluate_pipe(left_val, inner_left)?;
                self.evaluate_pipe(inner_val, inner_right)
            }
            _ => Err(format!("Right side of pipe must be a call or method call, got {:?}", right)),
        }
    }

    fn literal_to_value(&mut self, lit: &Literal) -> Result<Value, String> {
        match lit {
            Literal::String(s) => Ok(Value::String(s.clone())),
            Literal::Number(n) => Ok(Value::Number(*n)),
            Literal::Bool(b) => Ok(Value::Bool(*b)),
            Literal::Nil => Ok(Value::Nil),
            Literal::List(items) => {
                let mut values = Vec::new();
                for item in items { values.push(self.evaluate(item)?); }
                Ok(Value::List(values))
            }
            Literal::Dict(entries) => {
                let mut map = HashMap::new();
                for (key, expr) in entries {
                    map.insert(key.clone(), self.evaluate(expr)?);
                }
                Ok(Value::Dict(map))
            }
        }
    }

    fn call_function(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        match name {
            "print" => {
                let msg = args.into_iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\t");
                println!("{}", msg);
                Ok(Value::Nil)
            }
            "range" => {
                let start = args.get(0).and_then(|v| match v { Value::Number(n) => Some(*n as i64), _ => None }).unwrap_or(0);
                let end = args.get(1).and_then(|v| match v { Value::Number(n) => Some(*n as i64), _ => None }).unwrap_or(start);
                let step = args.get(2).and_then(|v| match v { Value::Number(n) => Some(*n as i64), _ => None }).unwrap_or(1);
                let mut items = Vec::new();
                let mut i = start;
                while i < end { items.push(Value::Number(i as f64)); i += step; }
                Ok(Value::List(items))
            }
            "len" => {
                let len = match args.get(0) {
                    Some(Value::List(list)) => list.len(),
                    Some(Value::String(s)) => s.len(),
                    Some(Value::Dict(map)) => map.len(),
                    _ => return Err("len() expects a list, string, or dict".to_string()),
                };
                Ok(Value::Number(len as f64))
            }
            _ => {
                // 先 clone 出值，释放 borrow，避免借用冲突
                let looked_up = self.environment.lock().unwrap().get(name).clone();
                if let Some(value) = looked_up {
                    match value {
                        Value::Task { params, body, .. } => {
                            let params = params.clone();
                            let body = body.clone();
                            self.call_task(&params, &body, args)
                        }
                        Value::Closure { params, body, env } => {
                            let params = params.clone();
                            let body = body.clone();
                            let env = env.clone();
                            self.call_closure(&params, &body, env, args)
                        }
                        _ => Err(format!("'{}' is not callable", name)),
                    }
                } else {
                    Err(format!("Undefined function or task: {}", name))
                }
            }
        }
    }

    fn call_task(&mut self, params: &[String], body: &[Stmt], args: Vec<Value>) -> Result<Value, String> {
        let env = Arc::new(Mutex::new(Environment::with_parent(self.globals.clone())));
        for (i, param) in params.iter().enumerate() {
            let value = args.get(i).cloned().unwrap_or(Value::Nil);
            env.lock().unwrap().define(param.clone(), value, false);
        }
        let result = self.execute_block(body, env)?;
        Ok(result.unwrap_or(Value::Nil))
    }

    fn call_closure(&mut self, params: &[String], body: &[Stmt], env: Arc<Mutex<Environment>>, args: Vec<Value>) -> Result<Value, String> {
        let call_env = Arc::new(Mutex::new(Environment::with_parent(env)));
        for (i, param) in params.iter().enumerate() {
            let value = args.get(i).cloned().unwrap_or(Value::Nil);
            call_env.lock().unwrap().define(param.clone(), value, false);
        }
        let result = self.execute_block(body, call_env)?;
        Ok(result.unwrap_or(Value::Nil))
    }

    fn call_method(&mut self, mut object: Value, method: &str, args: Vec<Value>) -> Result<Value, String> {
        match object {
            Value::List(list) => {
                match method {
                    "push" => {
                        let item = args.get(0).cloned().unwrap_or(Value::Nil);
                        let mut new_list = list.clone();
                        new_list.push(item);
                        Ok(Value::List(new_list))
                    }
                    "get" => {
                        let index = args.get(0).and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None }).unwrap_or(0);
                        Ok(list.get(index).cloned().unwrap_or(Value::Nil))
                    }
                    "pop" => {
                        let mut new_list = list.clone();
                        let item = new_list.pop().unwrap_or(Value::Nil);
                        Ok(item)
                    }
                    "len" => Ok(Value::Number(list.len() as f64)),
                    "map" => {
                        let mapper = args.get(0).cloned().ok_or("map() requires a function")?;
                        let mut result = Vec::new();
                        for item in list {
                            let mapped = self.call_value(&mapper, vec![item])?;
                            result.push(mapped);
                        }
                        Ok(Value::List(result))
                    }
                    "filter" => {
                        let predicate = args.get(0).cloned().ok_or("filter() requires a function")?;
                        let mut result = Vec::new();
                        for item in list {
                            let keep = self.call_value(&predicate, vec![item.clone()])?;
                            if is_truthy(&keep) {
                                result.push(item);
                            }
                        }
                        Ok(Value::List(result))
                    }
                    "reduce" => {
                        let reducer = args.get(0).cloned().ok_or("reduce() requires a function")?;
                        let mut acc = args.get(1).cloned().unwrap_or(Value::Nil);
                        for item in list {
                            acc = self.call_value(&reducer, vec![acc, item])?;
                        }
                        Ok(acc)
                    }
                    _ => Err(format!("List has no method: {}", method)),
                }
            }
            Value::Dict(map) => {
                match method {
                    "get" => {
                        let key = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        Ok(map.get(&key).cloned().unwrap_or(Value::Nil))
                    }
                    "set" => {
                        let key = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        let value = args.get(1).cloned().unwrap_or(Value::Nil);
                        let mut new_map = map.clone();
                        new_map.insert(key, value);
                        Ok(Value::Dict(new_map))
                    }
                    "keys" => {
                        let keys: Vec<Value> = map.keys().map(|k| Value::String(k.clone())).collect();
                        Ok(Value::List(keys))
                    }
                    "values" => {
                        let values: Vec<Value> = map.values().cloned().collect();
                        Ok(Value::List(values))
                    }
                    "len" => Ok(Value::Number(map.len() as f64)),
                    _ => Err(format!("Dict has no method: {}", method)),
                }
            }
            Value::Builtin(name) => match (name.as_str(), method) {
                ("ai", "chat") => {
                    let prompt = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                    // v10: 单轮无状态调用（向后兼容），支持环境变量配置
                    match env::var("OPENAI_API_KEY") {
                        Ok(key) if !key.is_empty() => {
                            let model = env::var("MORA_AI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
                            let base_url = env::var("MORA_AI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
                            let msgs = vec![("user".to_string(), prompt)];
                            self.real_ai_chat(&msgs, &key, &model, &base_url)
                        }
                        _ => {
                            eprintln!("[ai.chat mock — set OPENAI_API_KEY for real call] {}", prompt);
                            Ok(Value::String(format!("[Mock response for: {}]", prompt)))
                        }
                    }
                }
                ("ai", "create") => {
                    // v10: 创建多轮对话对象
                    let model = args.get(0).map(|v| v.to_string()).unwrap_or_else(|| "gpt-4o-mini".to_string());
                    let base_url = args.get(1).map(|v| v.to_string()).unwrap_or_else(|| "https://api.openai.com/v1".to_string());
                    match env::var("OPENAI_API_KEY") {
                        Ok(key) if !key.is_empty() => {
                            Ok(Value::Conversation {
                                messages: vec![],
                                model,
                                base_url,
                                api_key: key,
                            })
                        }
                        _ => Err("ai.create: set OPENAI_API_KEY environment variable for real calls".to_string()),
                    }
                }
                ("ai", "embed") => {
                    // v10 范围外：保持 mock
                    let text = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                    println!("[ai.embed mock] {}", text);
                    Ok(Value::String(format!("[Mock embedding for: {}]", text)))
                }
                ("web", "fetch") => {
                    let url = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                    // v10: 真实 HTTP GET
                    self.real_web_fetch(&url)
                }
                ("json", "parse") => {
                    // v10: 真实 JSON 解析
                    let text = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                    json_to_value(&text).map_err(|e| format!("json.parse: {}", e))
                }
                ("json", "stringify") => {
                    // v10: JSON 序列化
                    let value = args.get(0).cloned().unwrap_or(Value::Nil);
                    Ok(Value::String(value_to_json(&value)))
                }
                ("file", "read") => {
                    // v10 范围外：保持 mock
                    let path = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                    println!("[file.read mock] {}", path);
                    Ok(Value::String(format!("[Mock content of {}]", path)))
                }
                ("file", "write") => {
                    // v10 范围外：保持 mock
                    let path = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                    let content = args.get(1).map(|v| v.to_string()).unwrap_or_default();
                    println!("[file.write mock] {} -> {}", path, content);
                    Ok(Value::Nil)
                }
                _ => Err(format!("Unknown method: {}.{}", name, method)),
            },
            Value::Conversation { ref mut messages, ref model, ref base_url, ref api_key } => {
                match method {
                    "chat" => {
                        let prompt = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        if prompt.is_empty() {
                            return Err("conv.chat: prompt cannot be empty".to_string());
                        }
                        messages.push(("user".to_string(), prompt));
                        let api_key = api_key.clone();
                        let model = model.clone();
                        let base_url = base_url.clone();
                        let response = self.real_ai_chat(messages, &api_key, &model, &base_url)?;
                        messages.push(("assistant".to_string(), response.to_string()));
                        Ok(response)
                    }
                    "history" => {
                        let hist: Vec<Value> = messages.iter().map(|(role, content)| {
                            let mut m = HashMap::new();
                            m.insert("role".to_string(), Value::String(role.clone()));
                            m.insert("content".to_string(), Value::String(content.clone()));
                            Value::Dict(m)
                        }).collect();
                        Ok(Value::List(hist))
                    }
                    "clear" => {
                        messages.clear();
                        Ok(Value::Nil)
                    }
                    "model" => Ok(Value::String(model.clone())),
                    "len" => Ok(Value::Number(messages.len() as f64)),
                    _ => Err(format!("Conversation has no method: {}", method)),
                }
            }
            Value::String(s) => {
                match method {
                    "len" => Ok(Value::Number(s.len() as f64)),
                    "upper" => Ok(Value::String(s.to_uppercase())),
                    "lower" => Ok(Value::String(s.to_lowercase())),
                    "trim" => Ok(Value::String(s.trim().to_string())),
                    "starts_with" => {
                        let prefix = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        Ok(Value::Bool(s.starts_with(&prefix)))
                    }
                    "ends_with" => {
                        let suffix = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        Ok(Value::Bool(s.ends_with(&suffix)))
                    }
                    "contains" => {
                        let needle = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        Ok(Value::Bool(s.contains(&needle)))
                    }
                    "split" => {
                        let sep = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        let parts: Vec<Value> = s.split(&sep)
                            .map(|p| Value::String(p.to_string()))
                            .collect();
                        Ok(Value::List(parts))
                    }
                    "replace" => {
                        let from = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                        let to = args.get(1).map(|v| v.to_string()).unwrap_or_default();
                        Ok(Value::String(s.replace(&from, &to)))
                    }
                    _ => Err(format!("String has no method: {}", method)),
                }
            }
            _ => Err(format!("Can only call methods on lists, dicts, strings, conversations, or builtin objects")),
        }
    }

    fn call_value(&mut self, value: &Value, args: Vec<Value>) -> Result<Value, String> {
        match value {
            Value::Closure { params, body, env } => {
                let params = params.clone();
                let body = body.clone();
                let env = env.clone();
                self.call_closure(&params, &body, env, args)
            }
            Value::Task { params, body, .. } => {
                let params = params.clone();
                let body = body.clone();
                self.call_task(&params, &body, args)
            }
            _ => Err(format!("Value is not callable: {}", value)),
        }
    }

    // ===================================================================
    // v10: 真实 HTTP 客户端实现（基于 ureq）
    // ===================================================================

    /// 真实 HTTP GET 请求。失败时返回带上下文的错误信息。
    ///
    /// 设计要点：
    /// - 30s 读超时、10s 写超时（AI/网络条件下的合理值）
    /// - 4xx/5xx 状态码视为错误（语义：响应不可用）
    /// - 错误信息包含状态码 + URL + 响应体前 200 字符（便于排错）
    /// - 同步阻塞，与同步解释器天然契合
    fn real_web_fetch(&self, url: &str) -> Result<Value, String> {
        if url.is_empty() {
            return Err("web.fetch: URL cannot be empty".to_string());
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(format!(
                "web.fetch: URL must start with http:// or https://, got: {}",
                url
            ));
        }

        let agent = ureq::AgentBuilder::new()
            .timeout_read(Duration::from_secs(HTTP_READ_TIMEOUT_SECS))
            .timeout_write(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS))
            .build();

        match agent.get(url).call() {
            Ok(response) => match response.into_string() {
                Ok(text) => Ok(Value::String(text)),
                Err(e) => Err(format!("web.fetch: failed to read response body: {}", e)),
            },
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let excerpt: String = body.chars().take(200).collect();
                Err(format!(
                    "web.fetch: HTTP {} {} (body excerpt: {})",
                    status, url, excerpt
                ))
            }
            Err(ureq::Error::Transport(t)) => Err(format!(
                "web.fetch: network error for {}: {}",
                url, t
            )),
        }
    }

    /// 真实 Chat Completions API 调用（支持 OpenAI 兼容端点）。
    ///
    /// 关键设计：
    /// - **messages 参数**：完整对话历史，支持多轮上下文
    /// - **model / base_url 参数**：可配置，兼容本地模型和其他 API 提供商
    /// - **手写 JSON 请求体**：保持零 serde 依赖原则
    /// - **结构化 JSON 响应解析**：用 json_to_value 提取 choices[0].message.content
    /// - **同步阻塞**：60s 读超时（AI 推理可能慢）
    fn real_ai_chat(&self, messages: &[(String, String)], api_key: &str, model: &str, base_url: &str) -> Result<Value, String> {
        if messages.is_empty() {
            return Err("ai.chat: messages cannot be empty".to_string());
        }

        // 构建 messages JSON 数组
        let msgs_json: String = messages.iter().map(|(role, content)| {
            let escaped_content = content
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            format!(
                r#"{{"role":"{}","content":"{}"}}"#,
                role, escaped_content
            )
        }).collect::<Vec<_>>().join(",");

        let escaped_model = model
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let body = format!(
            r#"{{"model":"{}","messages":[{}]}}"#,
            escaped_model, msgs_json
        );

        let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));

        let agent = ureq::AgentBuilder::new()
            .timeout_read(Duration::from_secs(AI_READ_TIMEOUT_SECS))
            .timeout_write(Duration::from_secs(HTTP_WRITE_TIMEOUT_SECS))
            .build();

        match agent
            .post(&url)
            .set("Authorization", &format!("Bearer {}", api_key))
            .set("Content-Type", "application/json")
            .send_string(&body)
        {
            Ok(response) => match response.into_string() {
                Ok(text) => {
                    // 结构化 JSON 解析：提取 choices[0].message.content
                    self.extract_ai_content(&text)
                        .or_else(|_| Ok(Value::String(text)))
                }
                Err(e) => Err(format!("ai.chat: failed to read response body: {}", e)),
            },
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let excerpt: String = body.chars().take(300).collect();
                Err(format!(
                    "ai.chat: API error HTTP {} from {} (body: {})",
                    status, url, excerpt
                ))
            }
            Err(ureq::Error::Transport(t)) => Err(format!(
                "ai.chat: network error connecting to {}: {}",
                url, t
            )),
        }
    }

    /// 从 OpenAI 兼容 API 响应中提取 choices[0].message.content
    fn extract_ai_content(&self, json_text: &str) -> Result<Value, String> {
        let root = json_to_value(json_text)?;

        // root 应该是 Dict，提取 "choices" 数组
        if let Value::Dict(ref map) = root {
            if let Some(Value::List(choices)) = map.get("choices") {
                if let Some(first) = choices.first() {
                    if let Value::Dict(ref choice_map) = first {
                        // 标准格式: choices[0].message.content
                        if let Some(Value::Dict(ref msg_map)) = choice_map.get("message") {
                            if let Some(Value::String(content)) = msg_map.get("content") {
                                return Ok(Value::String(content.clone()));
                            }
                        }
                        // 兼容格式: choices[0].text (旧版 completions API)
                        if let Some(Value::String(text)) = choice_map.get("text") {
                            return Ok(Value::String(text.clone()));
                        }
                    }
                }
            }
            // 兼容某些 API 的顶层 "content" 字段
            if let Some(Value::String(content)) = map.get("content") {
                return Ok(Value::String(content.clone()));
            }
        }

        Err("Could not extract content from API response".to_string())
    }
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Nil => false,
        Value::Bool(b) => *b,
        _ => true,
    }
}

fn is_builtin_object(name: &str) -> bool {
    matches!(name, "ai" | "web" | "json" | "file")
}

/// 检查名称是否是可通过管道自动调用的方法（列表/字符串方法）
fn is_pipe_method(name: &str) -> bool {
    matches!(name,
        "map" | "filter" | "reduce" | "push" | "pop" | "get" | "len" |
        "upper" | "lower" | "trim" | "starts_with" | "ends_with" |
        "contains" | "split" | "replace"
    )
}

fn eval_binary(left: Value, op: &BinaryOp, right: Value) -> Result<Value, String> {
    match op {
        BinaryOp::Add => match (&left, &right) {
            (Value::Number(a), Value::Number(b)) => Ok(Value::Number(a + b)),
            (Value::String(a), Value::String(b)) => Ok(Value::String(format!("{}{}", a, b))),
            // 字符串 + 任意类型 → 自动转字符串拼接
            (Value::String(a), _) => Ok(Value::String(format!("{}{}", a, right))),
            (_, Value::String(b)) => Ok(Value::String(format!("{}{}", left, b))),
            (Value::List(a), Value::List(b)) => {
                let mut merged = a.clone();
                merged.extend(b.clone());
                Ok(Value::List(merged))
            }
            _ => Err("Operands must be two numbers, two strings, or two lists".to_string()),
        },
        BinaryOp::Sub => numeric_op(left, right, |a, b| a - b),
        BinaryOp::Mul => numeric_op(left, right, |a, b| a * b),
        BinaryOp::Div => numeric_op(left, right, |a, b| a / b),
        BinaryOp::Mod => numeric_op(left, right, |a, b| a % b),
        BinaryOp::Equal => Ok(Value::Bool(values_equal(&left, &right))),
        BinaryOp::NotEqual => Ok(Value::Bool(!values_equal(&left, &right))),
        BinaryOp::Greater => numeric_cmp(left, right, |a, b| a > b),
        BinaryOp::Less => numeric_cmp(left, right, |a, b| a < b),
        BinaryOp::GreaterEqual => numeric_cmp(left, right, |a, b| a >= b),
        BinaryOp::LessEqual => numeric_cmp(left, right, |a, b| a <= b),
    }
}

fn numeric_op<F>(left: Value, right: Value, op: F) -> Result<Value, String>
where F: Fn(f64, f64) -> f64 {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => Ok(Value::Number(op(a, b))),
        _ => Err("Operands must be numbers".to_string()),
    }
}

fn numeric_cmp<F>(left: Value, right: Value, op: F) -> Result<Value, String>
where F: Fn(f64, f64) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => Ok(Value::Bool(op(a, b))),
        _ => Err("Operands must be numbers".to_string()),
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Number(a), Value::Number(b)) => a == b,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::List(a), Value::List(b)) => a == b,
        (Value::Dict(a), Value::Dict(b)) => a == b,
        // Conversation 不支持相等比较——比较引用无意义
        _ => false,
    }
}

fn literal_to_value_static(lit: &Literal) -> Value {
    match lit {
        Literal::String(s) => Value::String(s.clone()),
        Literal::Number(n) => Value::Number(*n),
        Literal::Bool(b) => Value::Bool(*b),
        Literal::Nil => Value::Nil,
        Literal::List(_) => Value::Nil,
        Literal::Dict(_) => Value::Nil,
    }
}

fn check_type(value: &Value, hint: &str) -> bool {
    match (value, hint) {
        (Value::String(_), "string") => true,
        (Value::Number(_), "number") => true,
        (Value::Bool(_), "bool") => true,
        (Value::Nil, "nil") => true,
        (Value::List(_), "list") => true,
        (Value::Dict(_), "dict") => true,
        (Value::Task{..}, "task") => true,
        (Value::Conversation{..}, "conversation") => true,
        _ => false,
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "bool",
        Value::Nil => "nil",
        Value::List(_) => "list",
        Value::Dict(_) => "dict",
        Value::Task{..} => "task",
        Value::Closure{..} => "closure",
        Value::Builtin(_) => "builtin",
        Value::Conversation{..} => "conversation",
    }
}

// --- JSON serialization ---

fn value_to_json(value: &Value) -> String {
    match value {
        Value::String(s) => format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")),
        Value::Number(n) => {
            if n.fract() == 0.0 {
                format!("{:.0}", n)
            } else {
                format!("{}", n)
            }
        }
        Value::Bool(b) => b.to_string(),
        Value::Nil => "null".to_string(),
        Value::List(items) => {
            let parts: Vec<String> = items.iter().map(value_to_json).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Dict(map) => {
            let parts: Vec<String> = map.iter()
                .map(|(k, v)| format!("\"{}\":{}", k, value_to_json(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Task { .. } => "null".to_string(),
        Value::Closure { .. } => "null".to_string(),
        Value::Builtin(_) => "null".to_string(),
        Value::Conversation { .. } => "null".to_string(),
    }
}

fn json_to_value(json: &str) -> Result<Value, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Err("Empty JSON".to_string());
    }
    parse_json_value(trimmed).map(|(v, _)| v)
}

fn parse_json_value(s: &str) -> Result<(Value, usize), String> {
    let s = s.trim_start();
    let first = s.chars().next().ok_or("Empty string")?;

    if first == '"' {
        parse_json_string(s)
    } else if first == '[' {
        parse_json_list(s)
    } else if first == '{' {
        parse_json_dict(s)
    } else if first == 't' || first == 'f' {
        parse_json_bool(s)
    } else if first == 'n' {
        parse_json_null(s)
    } else if first.is_ascii_digit() || first == '-' {
        parse_json_number(s)
    } else {
        Err(format!("Unexpected JSON character: {}", first))
    }
}

fn parse_json_string(s: &str) -> Result<(Value, usize), String> {
    let mut i = 1;
    let mut result = String::new();
    let chars: Vec<char> = s.chars().collect();
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            match chars[i + 1] {
                '"' => { result.push('"'); i += 2; }
                '\\' => { result.push('\\'); i += 2; }
                'n' => { result.push('\n'); i += 2; }
                't' => { result.push('\t'); i += 2; }
                _ => { result.push(chars[i + 1]); i += 2; }
            }
        } else if chars[i] == '"' {
            return Ok((Value::String(result), i + 1));
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }
    Err("Unterminated string".to_string())
}

fn parse_json_number(s: &str) -> Result<(Value, usize), String> {
    let mut i = 0;
    let chars: Vec<char> = s.chars().collect();
    if chars[i] == '-' { i += 1; }
    while i < chars.len() && chars[i].is_ascii_digit() { i += 1; }
    if i < chars.len() && chars[i] == '.' {
        i += 1;
        while i < chars.len() && chars[i].is_ascii_digit() { i += 1; }
    }
    let num_str: String = chars[0..i].iter().collect();
    let num: f64 = num_str.parse().map_err(|_| "Invalid number")?;
    Ok((Value::Number(num), i))
}

fn parse_json_bool(s: &str) -> Result<(Value, usize), String> {
    if s.starts_with("true") {
        Ok((Value::Bool(true), 4))
    } else if s.starts_with("false") {
        Ok((Value::Bool(false), 5))
    } else {
        Err("Invalid boolean".to_string())
    }
}

fn parse_json_null(s: &str) -> Result<(Value, usize), String> {
    if s.starts_with("null") {
        Ok((Value::Nil, 4))
    } else {
        Err("Invalid null".to_string())
    }
}

fn parse_json_list(s: &str) -> Result<(Value, usize), String> {
    let mut i = 1; // skip '['
    let mut items = Vec::new();
    let chars: Vec<char> = s.chars().collect();

    while i < chars.len() {
        while i < chars.len() && chars[i].is_ascii_whitespace() { i += 1; }
        if i < chars.len() && chars[i] == ']' {
            return Ok((Value::List(items), i + 1));
        }
        let rest: String = chars[i..].iter().collect();
        let (val, consumed) = parse_json_value(&rest)?;
        items.push(val);
        i += consumed;
        while i < chars.len() && chars[i].is_ascii_whitespace() { i += 1; }
        if i < chars.len() && chars[i] == ',' {
            i += 1;
        } else if i < chars.len() && chars[i] == ']' {
            return Ok((Value::List(items), i + 1));
        }
    }
    Err("Unterminated list".to_string())
}

fn parse_json_dict(s: &str) -> Result<(Value, usize), String> {
    let mut i = 1; // skip '{'
    let mut map = HashMap::new();
    let chars: Vec<char> = s.chars().collect();

    while i < chars.len() {
        while i < chars.len() && chars[i].is_ascii_whitespace() { i += 1; }
        if i < chars.len() && chars[i] == '}' {
            return Ok((Value::Dict(map), i + 1));
        }

        let rest: String = chars[i..].iter().collect();
        let (key_val, key_consumed) = parse_json_string(&rest)?;
        let key = match key_val {
            Value::String(s) => s,
            _ => return Err("Dict key must be string".to_string()),
        };
        i += key_consumed;

        while i < chars.len() && chars[i].is_ascii_whitespace() { i += 1; }
        if i >= chars.len() || chars[i] != ':' {
            return Err("Expected ':' after dict key".to_string());
        }
        i += 1;

        let rest: String = chars[i..].iter().collect();
        let (val, consumed) = parse_json_value(&rest)?;
        map.insert(key, val);
        i += consumed;

        while i < chars.len() && chars[i].is_ascii_whitespace() { i += 1; }
        if i < chars.len() && chars[i] == ',' {
            i += 1;
        } else if i < chars.len() && chars[i] == '}' {
            return Ok((Value::Dict(map), i + 1));
        }
    }
    Err("Unterminated dict".to_string())
}
