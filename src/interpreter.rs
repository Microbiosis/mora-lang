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
pub struct Environment {
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

    pub fn execute(&mut self, stmt: &Stmt) -> Result<FlowSignal, String> {
        match stmt {
            Stmt::Let { name, type_hint, init, exported, span: _ } => {
                let value = self.evaluate(init)?;
                if let Some(hint) = type_hint {
                    if !check_type(&value, hint) {
                        return Err(format!("Type mismatch: expected {}, got {}", hint, type_name(&value)));
                    }
                }
                self.environment.lock().unwrap().define(name.clone(), value, *exported);
                Ok(FlowSignal::None)
            }
            Stmt::Assign { name, value, span: _ } => {
                let val = self.evaluate(value)?;
                if !self.environment.lock().unwrap().assign(name, val.clone()) {
                    self.environment.lock().unwrap().define(name.clone(), val, false);
                }
                Ok(FlowSignal::None)
            }
            Stmt::IndexAssign { object, index, value, span: _ } => {
                let obj = self.evaluate(object)?;
                let idx = self.evaluate(index)?;
                let val = self.evaluate(value)?;
                match (&obj, &idx) {
                    (Value::List(list), Value::Number(n)) => {
                        let i = *n as usize;
                        if i < list.len() {
                            let mut new_list = list.clone();
                            new_list[i] = val;
                            Ok(FlowSignal::None)
                        } else {
                            Err(format!("Index out of bounds: {} (len: {})", i, list.len()))
                        }
                    }
                    _ => Err("Can only index assign to lists".to_string()),
                }
            }
            Stmt::TaskDef { name, params, return_type: _, body, exported, span: _ } => {
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                let task = Value::Task { name: name.clone(), params: param_names, body: body.clone() };
                self.environment.lock().unwrap().define(name.clone(), task, *exported);
                Ok(FlowSignal::None)
            }
            Stmt::If { condition, then_branch, span: _ } => {
                let cond = self.evaluate(condition)?;
                if is_truthy(&cond) {
                    let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                    // return 信号必须穿透 if 边界向外冒泡
                    self.execute_block(then_branch, env)
                } else {
                    Ok(FlowSignal::None)
                }
            }
            Stmt::For { var, var_type: _, iterable, body, span: _ } => {
                let iter_val = self.evaluate(iterable)?;
                // return 信号必须穿透 for 边界向外冒泡（每次迭代后检查）
                match iter_val {
                    Value::List(items) => {
                        for item in items {
                            let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                            env.lock().unwrap().define(var.clone(), item, false);
                            let signal = self.execute_block(body, env)?;
                            if signal.is_return() { return Ok(signal); }
                        }
                        Ok(FlowSignal::None)
                    }
                    Value::String(s) => {
                        for ch in s.chars() {
                            let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                            env.lock().unwrap().define(var.clone(), Value::String(ch.to_string()), false);
                            let signal = self.execute_block(body, env)?;
                            if signal.is_return() { return Ok(signal); }
                        }
                        Ok(FlowSignal::None)
                    }
                    _ => Err(format!("Cannot iterate over {}", iter_val)),
                }
            }
            Stmt::Try { try_block, catch_var, catch_block, span: _ } => {
                let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                match self.execute_block(try_block, env.clone()) {
                    // 运行时错误：进 catch。**return 信号不算错误**，直接穿透。
                    Ok(signal @ FlowSignal::Return(_)) => Ok(signal),
                    Ok(FlowSignal::None) => Ok(FlowSignal::None),
                    Err(err_msg) => {
                        env.lock().unwrap().define(catch_var.clone(), Value::String(err_msg), false);
                        // catch 块内若有 return 也要穿透
                        self.execute_block(catch_block, env)
                    }
                }
            }
            Stmt::Import { path, span: _ } => {
                let module_env = self.import_module(path)?;
                let exports = module_env.lock().unwrap().exports.clone();
                for (name, value) in exports {
                    self.environment.lock().unwrap().define(name, value, false);
                }
                Ok(FlowSignal::None)
            }
            Stmt::Parallel { stmts, span: _ } => {
                self.execute_parallel(stmts)
            }
            Stmt::Match { expr, arms, span: _ } => {
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
            Stmt::Save { path, value, span: _ } => {
                let path_val = self.evaluate(path)?;
                let data_val = self.evaluate(value)?;
                let path_str = match path_val {
                    Value::String(s) => s,
                    _ => return Err("save path must be a string".to_string()),
                };
                let json = value_to_json(&data_val);
                fs::write(&path_str, json).map_err(|e| format!("Failed to save: {}", e))?;
                println!("[save] {} -> {}", path_str, type_name(&data_val));
                Ok(FlowSignal::None)
            }
            Stmt::Load { path, var, span: _ } => {
                let path_val = self.evaluate(path)?;
                let path_str = match path_val {
                    Value::String(s) => s,
                    _ => return Err("load path must be a string".to_string()),
                };
                let json = fs::read_to_string(&path_str).map_err(|e| format!("Failed to load: {}", e))?;
                let value = json_to_value(&json)?;
                self.environment.lock().unwrap().define(var.clone(), value, false);
                println!("[load] {} -> {}", path_str, var);
                Ok(FlowSignal::None)
            }
            Stmt::ReadFile { path, var, span: _ } => {
                // v11: read "path" into var  →  等价于 let var = file.read_text("path")
                let path_val = self.evaluate(path)?;
                let path_str = expect_string(path_val, "read path")?;
                let content = std::fs::read_to_string(&path_str)
                    .map_err(|e| format!("read: cannot read '{}': {}", path_str, e))?;
                self.environment.lock().unwrap().define(var.clone(), Value::String(content), false);
                println!("[read] {} -> {}", path_str, var);
                Ok(FlowSignal::None)
            }
            Stmt::WriteFile { path, content, span: _ } => {
                // v11: write "path", content  →  等价于 file.write_text("path", content)
                let path_val = self.evaluate(path)?;
                let content_val = self.evaluate(content)?;
                let path_str = expect_string(path_val, "write path")?;
                let content_str = expect_string(content_val, "write content")?;
                if let Some(parent) = std::path::Path::new(&path_str).parent() {
                    if !parent.as_os_str().is_empty() && !parent.exists() {
                        return Err(format!(
                            "write: parent directory does not exist: {}",
                            parent.display()
                        ));
                    }
                }
                std::fs::write(&path_str, &content_str)
                    .map_err(|e| format!("write: cannot write '{}': {}", path_str, e))?;
                println!("[write] {}", path_str);
                Ok(FlowSignal::None)
            }
            Stmt::AppendFile { path, content, span: _ } => {
                // v11: append "path", content
                let path_val = self.evaluate(path)?;
                let content_val = self.evaluate(content)?;
                let path_str = expect_string(path_val, "append path")?;
                let content_str = expect_string(content_val, "append content")?;
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path_str)
                    .map_err(|e| format!("append: cannot open '{}': {}", path_str, e))?;
                f.write_all(content_str.as_bytes())
                    .map_err(|e| format!("append: cannot write '{}': {}", path_str, e))?;
                println!("[append] {}", path_str);
                Ok(FlowSignal::None)
            }
            Stmt::ReadBytesFile { path, var, span: _ } => {
                // v11: read_bytes "path" into var  →  var 是 hex 字符串
                let path_val = self.evaluate(path)?;
                let path_str = expect_string(path_val, "read_bytes path")?;
                let bytes = std::fs::read(&path_str)
                    .map_err(|e| format!("read_bytes: cannot read '{}': {}", path_str, e))?;
                self.environment.lock().unwrap()
                    .define(var.clone(), Value::String(hex_encode(&bytes)), false);
                println!("[read_bytes] {} -> {} ({} bytes)", path_str, var, bytes.len());
                Ok(FlowSignal::None)
            }
            Stmt::WriteBytesFile { path, content, span: _ } => {
                // v11: write_bytes "path", hex
                let path_val = self.evaluate(path)?;
                let content_val = self.evaluate(content)?;
                let path_str = expect_string(path_val, "write_bytes path")?;
                let hex = expect_string(content_val, "write_bytes content")?;
                let bytes = hex_decode(&hex)
                    .map_err(|e| format!("write_bytes: {}", e))?;
                std::fs::write(&path_str, &bytes)
                    .map_err(|e| format!("write_bytes: cannot write '{}': {}", path_str, e))?;
                println!("[write_bytes] {} ({} bytes)", path_str, bytes.len());
                Ok(FlowSignal::None)
            }
            Stmt::Return { value, span: _ } => {
                let val = match value {
                    Some(expr) => self.evaluate(expr)?,
                    None => Value::Nil,
                };
                Ok(FlowSignal::Return(val))
            }
            Stmt::Expr(expr) => {
                // 副作用表达式（print、let mut、call 等），求值后不携带任何信号
                let _val = self.evaluate(expr)?;
                Ok(FlowSignal::None)
            }
        }
    }

    fn execute_block(&mut self, stmts: &[Stmt], env: Arc<Mutex<Environment>>) -> Result<FlowSignal, String> {
        let previous = self.environment.clone();
        self.environment = env;
        let mut last = FlowSignal::None;
        for stmt in stmts {
            last = self.execute(stmt)?;
            // 任何 return 信号立即停止块执行
            if last.is_return() {
                break;
            }
        }
        self.environment = previous;
        Ok(last)
    }

    fn execute_parallel(&mut self, stmts: &[Stmt]) -> Result<FlowSignal, String> {
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
                    Ok(Ok(signal)) => {
                        // FlowSignal::Return(val) → val（线程内 return 的值）
                        // FlowSignal::None → nil
                        values.push(signal.into_value());
                    }
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

        Ok(FlowSignal::None)
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
            Expr::Variable(name, _) => {
                let value = self.environment.lock().unwrap().get(name);
                match value {
                    Some(v) => Ok(v),
                    None if is_builtin_object(name) => Ok(Value::Builtin(name.clone())),
                    None => Err(format!("Undefined variable: {}", name)),
                }
            }
            Expr::Grouping(expr, _) => self.evaluate(expr),
            Expr::Binary { left, op, right, span: _ } => {
                let left = self.evaluate(left)?;
                let right = self.evaluate(right)?;
                eval_binary(left, op, right)
            }
            Expr::Pipe { left, right, span: _ } => {
                let left_val = self.evaluate(left)?;
                self.evaluate_pipe(left_val, right)
            }
            Expr::Call { callee, args, span: _ } => {
                let arg_values: Result<Vec<Value>, String> = args.iter().map(|a| self.evaluate(a.as_ref())).collect();
                self.call_function(callee, arg_values?)
            }
            Expr::MethodCall { object, method, args, span: _ } => {
                let obj = self.evaluate(object)?;
                let arg_values: Result<Vec<Value>, String> = args.iter().map(|a| self.evaluate(a.as_ref())).collect();
                self.call_method(obj, method, arg_values?)
            }
            Expr::Index { object, index, span: _ } => {
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
            Expr::Closure { params, return_type: _, body, span: _ } => {
                let param_names: Vec<String> = params.iter().map(|(n, _)| n.clone()).collect();
                Ok(Value::Closure {
                    params: param_names,
                    body: body.clone(),
                    env: self.environment.clone(),
                })
            }
            Expr::Match { expr, arms, span: _ } => {
                let val = self.evaluate(expr)?;
                for (pattern, arm_expr) in arms.iter() {
                    if let Some(bindings) = self.match_pattern(pattern, &val) {
                        let env = Arc::new(Mutex::new(Environment::with_parent(self.environment.clone())));
                        for (name, value) in bindings {
                            env.lock().unwrap().define(name, value, false);
                        }
                        let previous = self.environment.clone();
                        self.environment = env;
                        let result = self.evaluate(arm_expr.as_ref());
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
            Expr::Call { callee, args, span: _ } => {
                // 检查是否是列表/字符串方法名——自动转为方法调用
                if is_pipe_method(callee) {
                    let mut arg_values: Vec<Value> = Vec::new();
                    for arg in args {
                        arg_values.push(self.evaluate(arg.as_ref())?);
                    }
                    return self.call_method(left_val, callee, arg_values);
                }
                let mut arg_values: Vec<Value> = vec![left_val];
                for arg in args {
                    arg_values.push(self.evaluate(arg.as_ref())?);
                }
                self.call_function(callee, arg_values)
            }
            Expr::MethodCall { object, method, args, span: _ } => {
                let obj = self.evaluate(object)?;
                let mut arg_values: Vec<Value> = vec![left_val];
                for arg in args {
                    arg_values.push(self.evaluate(arg.as_ref())?);
                }
                self.call_method(obj, method, arg_values)
            }
            Expr::Variable(name, _) => {
                self.call_function(name, vec![left_val])
            }
            Expr::Pipe { left: inner_left, right: inner_right, span: _ } => {
                let inner_val = self.evaluate_pipe(left_val, inner_left)?;
                self.evaluate_pipe(inner_val, inner_right)
            }
            _ => Err(format!("Right side of pipe must be a call or method call, got {:?}", right)),
        }
    }

    fn literal_to_value(&mut self, lit: &Literal) -> Result<Value, String> {
        match lit {
            Literal::String(s, _) => Ok(Value::String(s.clone())),
            Literal::Number(n, _) => Ok(Value::Number(*n)),
            Literal::Bool(b, _) => Ok(Value::Bool(*b)),
            Literal::Nil(_) => Ok(Value::Nil),
            Literal::List(items, _) => {
                let mut values = Vec::new();
                for item in items { values.push(self.evaluate(item.as_ref())?); }
                Ok(Value::List(values))
            }
            Literal::Dict(entries, _) => {
                let mut map = HashMap::new();
                for (key, expr) in entries {
                    map.insert(key.clone(), self.evaluate(expr.as_ref())?);
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
        let signal = self.execute_block(body, env)?;
        // FlowSignal::Return(val) → 函数返回值 val
        // FlowSignal::None → 函数未显式 return，默认为 nil
        Ok(signal.into_value())
    }

    fn call_closure(&mut self, params: &[String], body: &[Stmt], env: Arc<Mutex<Environment>>, args: Vec<Value>) -> Result<Value, String> {
        let call_env = Arc::new(Mutex::new(Environment::with_parent(env)));
        for (i, param) in params.iter().enumerate() {
            let value = args.get(i).cloned().unwrap_or(Value::Nil);
            call_env.lock().unwrap().define(param.clone(), value, false);
        }
        // 闭包是**表达式**：body 通常是 [Stmt::Expr(expr)] 或 [Stmt::Return(val)]
        // 求值约定（与 task 不同——task 必须显式 return 才返回值）：
        //   1. 单条 Stmt::Expr(expr) → evaluate(expr) 作为闭包返回值
        //   2. 含 Stmt::Return(val) → val 是闭包返回值
        //   3. 其他（多 stmt / let / if 等）→ nil
        //
        // 我们用 execute_block 跑所有 stmt 收集副作用，
        // 然后手动取最后一条 expr 的值（如果有）。
        if let Some(Stmt::Return { value: _, span: _ }) = body.last() {
            let signal = self.execute_block(body, call_env)?;
            return Ok(signal.into_value());
        }
        // 单条 expr 闭包：单独 evaluate 取值（不能走 execute_block，因为
        // Stmt::Expr 现在返回 FlowSignal::None 不携带值）
        if body.len() == 1 {
            if let Stmt::Expr(expr) = &body[0] {
                let previous = self.environment.clone();
                self.environment = call_env;
                let result = self.evaluate(expr);
                self.environment = previous;
                return result;
            }
        }
        // 多 stmt 闭包：执行全部，最后如果有 expr 取值
        let previous = self.environment.clone();
        self.environment = call_env.clone();
        let mut last_expr_value = Value::Nil;
        for stmt in body {
            // 已经走过上一条 early return 路径
            if let Stmt::Expr(expr) = stmt {
                last_expr_value = self.evaluate(expr)?;
            } else {
                self.execute(stmt)?;
            }
        }
        self.environment = previous;
        Ok(last_expr_value)
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
                ("ai", method) => self.call_ai_method(method, &args),
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
                ("file", method) => self.call_file_method(method, &args),
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
    // v11: file.* 内建模块 — 完整文件系统能力
    // ===================================================================
    //
    // 设计要点：
    // - 文本 IO 用 String 承载；二进制 IO 用 hex 字符串承载（Mora 无原生 bytes 类型）
    // - 所有错误通过 Err 返回，调用方通过 try/catch 处理
    // - 路径参数统一为字符串，沿用 fs::read_to_string 等 std 行为
    // - 不做沙箱：Mora 是本地脚本语言，访问受 OS 文件权限保护
    // - hex 编解码用小写字母，与 web.fetch 等 JSON/HTTP 行为保持一致
    fn call_file_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        let expect_str = |idx: usize, name: &str| -> Result<String, String> {
            match args.get(idx) {
                Some(Value::String(s)) => Ok(s.clone()),
                Some(_) => Err(format!("file.{}: {} must be a string", method, name)),
                None => Err(format!("file.{}: missing argument {}", method, name)),
            }
        };
        match method {
            // ---- 文本 IO ----
            "read_text" => {
                let path = expect_str(0, "path")?;
                let content = std::fs::read_to_string(&path)
                    .map_err(|e| format!("file.read_text: cannot read '{}': {}", path, e))?;
                Ok(Value::String(content))
            }
            "write_text" => {
                let path = expect_str(0, "path")?;
                let content = expect_str(1, "content")?;
                if let Some(parent) = std::path::Path::new(&path).parent() {
                    if !parent.as_os_str().is_empty() && !parent.exists() {
                        return Err(format!(
                            "file.write_text: parent directory does not exist: {}",
                            parent.display()
                        ));
                    }
                }
                std::fs::write(&path, &content)
                    .map_err(|e| format!("file.write_text: cannot write '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "append_text" => {
                let path = expect_str(0, "path")?;
                let content = expect_str(1, "content")?;
                use std::io::Write;
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .map_err(|e| format!("file.append_text: cannot open '{}': {}", path, e))?;
                f.write_all(content.as_bytes())
                    .map_err(|e| format!("file.append_text: cannot write '{}': {}", path, e))?;
                Ok(Value::Nil)
            }

            // ---- 二进制 IO（hex 字符串承载）----
            "read_bytes" => {
                let path = expect_str(0, "path")?;
                let bytes = std::fs::read(&path)
                    .map_err(|e| format!("file.read_bytes: cannot read '{}': {}", path, e))?;
                Ok(Value::String(hex_encode(&bytes)))
            }
            "write_bytes" => {
                let path = expect_str(0, "path")?;
                let hex = expect_str(1, "hex")?;
                let bytes = hex_decode(&hex)
                    .map_err(|e| format!("file.write_bytes: {}", e))?;
                std::fs::write(&path, &bytes)
                    .map_err(|e| format!("file.write_bytes: cannot write '{}': {}", path, e))?;
                Ok(Value::Nil)
            }

            // ---- 元信息 ----
            "exists" => {
                let path = expect_str(0, "path")?;
                Ok(Value::Bool(std::path::Path::new(&path).exists()))
            }
            "is_file" => {
                let path = expect_str(0, "path")?;
                Ok(Value::Bool(std::path::Path::new(&path).is_file()))
            }
            "is_dir" => {
                let path = expect_str(0, "path")?;
                Ok(Value::Bool(std::path::Path::new(&path).is_dir()))
            }
            "size" => {
                let path = expect_str(0, "path")?;
                let meta = std::fs::metadata(&path)
                    .map_err(|e| format!("file.size: cannot stat '{}': {}", path, e))?;
                Ok(Value::Number(meta.len() as f64))
            }

            // ---- 目录操作 ----
            "list" => {
                let path = expect_str(0, "path")?;
                let entries = std::fs::read_dir(&path)
                    .map_err(|e| format!("file.list: cannot read dir '{}': {}", path, e))?;
                let mut names: Vec<String> = Vec::new();
                for entry in entries {
                    let entry = entry.map_err(|e| format!("file.list: {}", e))?;
                    names.push(entry.file_name().to_string_lossy().to_string());
                }
                names.sort();
                let items: Vec<Value> = names.into_iter().map(Value::String).collect();
                Ok(Value::List(items))
            }
            "mkdir" => {
                let path = expect_str(0, "path")?;
                std::fs::create_dir(&path)
                    .map_err(|e| format!("file.mkdir: cannot create '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "mkdir_all" => {
                let path = expect_str(0, "path")?;
                std::fs::create_dir_all(&path)
                    .map_err(|e| format!("file.mkdir_all: cannot create '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "remove" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                if p.is_dir() {
                    std::fs::remove_dir(&path)
                        .map_err(|e| format!("file.remove: cannot remove dir '{}': {}", path, e))?;
                } else {
                    std::fs::remove_file(&path)
                        .map_err(|e| format!("file.remove: cannot remove file '{}': {}", path, e))?;
                }
                Ok(Value::Nil)
            }
            "remove_all" => {
                let path = expect_str(0, "path")?;
                std::fs::remove_dir_all(&path)
                    .map_err(|e| format!("file.remove_all: cannot remove '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "rename" => {
                let from = expect_str(0, "from")?;
                let to = expect_str(1, "to")?;
                std::fs::rename(&from, &to)
                    .map_err(|e| format!("file.rename: cannot rename '{}' -> '{}': {}", from, to, e))?;
                Ok(Value::Nil)
            }
            "copy" => {
                let from = expect_str(0, "from")?;
                let to = expect_str(1, "to")?;
                std::fs::copy(&from, &to)
                    .map_err(|e| format!("file.copy: cannot copy '{}' -> '{}': {}", from, to, e))?;
                Ok(Value::Nil)
            }
            "touch" => {
                // v11 补充：创建空文件 / 确保文件存在
                // 注意：因 Mora 仅依赖 ureq 标准库，Unix `touch` 的"更新 mtime"语义
                // 在本实现中降级为"若已存在则 no-op"。需要真实 mtime 更新请改用 `file.write_text(path, "")`。
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                if !p.exists() {
                    if let Some(parent) = p.parent() {
                        if !parent.as_os_str().is_empty() && !parent.exists() {
                            return Err(format!(
                                "file.touch: parent directory does not exist: {}",
                                parent.display()
                            ));
                        }
                    }
                    std::fs::write(&path, "")
                        .map_err(|e| format!("file.touch: cannot create '{}': {}", path, e))?;
                }
                Ok(Value::Nil)
            }

            // ---- 路径与工作目录 ----
            "cwd" => {
                let cwd = std::env::current_dir()
                    .map_err(|e| format!("file.cwd: {}", e))?;
                Ok(Value::String(cwd.to_string_lossy().to_string()))
            }
            "chdir" => {
                let path = expect_str(0, "path")?;
                std::env::set_current_dir(&path)
                    .map_err(|e| format!("file.chdir: cannot chdir to '{}': {}", path, e))?;
                Ok(Value::Nil)
            }
            "home_dir" => {
                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .map_err(|_| "file.home_dir: HOME/USERPROFILE not set".to_string())?;
                Ok(Value::String(home))
            }
            "join" => {
                // 跨平台路径拼接
                let mut pb = std::path::PathBuf::new();
                for arg in args {
                    match arg {
                        Value::String(s) => pb.push(s),
                        _ => return Err(format!("file.join: all arguments must be strings")),
                    }
                }
                Ok(Value::String(pb.to_string_lossy().to_string()))
            }
            "abs" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                let abs = if p.is_absolute() {
                    p.to_path_buf()
                } else {
                    std::env::current_dir()
                        .map_err(|e| format!("file.abs: {}", e))?
                        .join(p)
                };
                Ok(Value::String(abs.to_string_lossy().to_string()))
            }
            "basename" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                let name = p.file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                Ok(Value::String(name))
            }
            "dirname" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                let parent = p.parent()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                Ok(Value::String(parent))
            }
            "extname" => {
                let path = expect_str(0, "path")?;
                let p = std::path::Path::new(&path);
                let ext = p.extension()
                    .map(|s| format!(".{}", s.to_string_lossy()))
                    .unwrap_or_default();
                Ok(Value::String(ext))
            }

            _ => Err(format!("file.{}: unknown method", method)),
        }
    }

    // ===================================================================
    // v11: ai.* — 向量嵌入、相似度、语义检索
    // ===================================================================
    fn call_ai_method(&self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "embed" => {
                // ai.embed(text | list_of_text, dim?) -> List<Number> | List<List<Number>>
                let first = args.get(0).cloned().unwrap_or(Value::Nil);
                let dim = match args.get(1) {
                    Some(Value::Number(n)) if *n > 0.0 => Some(*n as u32),
                    Some(Value::Number(n)) if *n == 0.0 => None,
                    Some(_) => return Err("ai.embed: dimensions must be a positive number".to_string()),
                    None => None,
                };
                let api_key = env::var("OPENAI_API_KEY")
                    .map_err(|_| "ai.embed: set OPENAI_API_KEY environment variable".to_string())?;
                let model = env::var("MORA_EMBED_MODEL")
                    .unwrap_or_else(|_| "text-embedding-3-small".to_string());
                let base_url = env::var("MORA_AI_BASE_URL")
                    .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());

                match first {
                    Value::String(s) => {
                        // 单文本
                        real_ai_embed_strings(&[s], &api_key, &model, &base_url, dim)
                    }
                    Value::List(items) => {
                        // 批量：必须全为字符串
                        let mut strs = Vec::with_capacity(items.len());
                        for it in &items {
                            match it {
                                Value::String(s) => strs.push(s.clone()),
                                _ => return Err("ai.embed: list elements must be strings".to_string()),
                            }
                        }
                        if strs.is_empty() {
                            return Err("ai.embed: list is empty".to_string());
                        }
                        real_ai_embed_strings(&strs, &api_key, &model, &base_url, dim)
                    }
                    _ => Err("ai.embed: first arg must be a string or list of strings".to_string()),
                }
            }
            "cosine" => {
                // ai.cosine(vec_a, vec_b) -> Number [-1, 1]
                let a = value_list_to_f64(args.get(0).unwrap_or(&Value::Nil), "ai.cosine")?;
                let b = value_list_to_f64(args.get(1).unwrap_or(&Value::Nil), "ai.cosine")?;
                Ok(Value::Number(cosine_similarity(&a, &b)?))
            }
            "dot" => {
                let a = value_list_to_f64(args.get(0).unwrap_or(&Value::Nil), "ai.dot")?;
                let b = value_list_to_f64(args.get(1).unwrap_or(&Value::Nil), "ai.dot")?;
                Ok(Value::Number(dot_product(&a, &b)?))
            }
            "euclidean" => {
                let a = value_list_to_f64(args.get(0).unwrap_or(&Value::Nil), "ai.euclidean")?;
                let b = value_list_to_f64(args.get(1).unwrap_or(&Value::Nil), "ai.euclidean")?;
                Ok(Value::Number(euclidean_distance(&a, &b)?))
            }
            "norm" => {
                let a = value_list_to_f64(args.get(0).unwrap_or(&Value::Nil), "ai.norm")?;
                Ok(Value::Number(l2_norm(&a)))
            }
            "search" => {
                // ai.search(query, corpus, k) -> List<{text, score, index}>
                // MVP：同步现场 embedding + cosine 排序。无 API key 时使用 mock 词袋兜底。
                self.ai_search_mvp(args)
            }
            _ => Err(format!("ai.{}: unknown method", method)),
        }
    }

    /// ai.search MVP：现场对 query + corpus 各调一次 embed，按 cosine 排序取 top-k
    fn ai_search_mvp(&self, args: &[Value]) -> Result<Value, String> {
        let query = match args.get(0) {
            Some(Value::String(s)) => s.clone(),
            _ => return Err("ai.search: first arg must be a string (query)".to_string()),
        };
        let corpus = match args.get(1) {
            Some(Value::List(items)) => {
                let mut strs = Vec::with_capacity(items.len());
                for it in items {
                    match it {
                        Value::String(s) => strs.push(s.clone()),
                        _ => return Err("ai.search: corpus elements must be strings".to_string()),
                    }
                }
                strs
            }
            _ => return Err("ai.search: second arg must be a list of strings (corpus)".to_string()),
        };
        let k = match args.get(2) {
            Some(Value::Number(n)) if *n > 0.0 => (*n as usize).min(corpus.len()),
            _ => corpus.len(),
        };
        if corpus.is_empty() {
            return Err("ai.search: corpus is empty".to_string());
        }

        // 准备输入：query + corpus
        let mut inputs = Vec::with_capacity(corpus.len() + 1);
        inputs.push(query.clone());
        inputs.extend(corpus.iter().cloned());

        // 调用 embed
        let has_key = env::var("OPENAI_API_KEY").map(|k| !k.is_empty()).unwrap_or(false);
        let embeddings = if has_key {
            let api_key = env::var("OPENAI_API_KEY").unwrap();
            let model = env::var("MORA_EMBED_MODEL")
                .unwrap_or_else(|_| "text-embedding-3-small".to_string());
            let base_url = env::var("MORA_AI_BASE_URL")
                .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
            let result = real_ai_embed_strings(&inputs, &api_key, &model, &base_url, None)?;
            // result 是 List<List<Number>>（因为 inputs.len() > 1）
            match result {
                Value::List(embs) => {
                    let mut out = Vec::with_capacity(embs.len());
                    for e in embs {
                        if let Ok(v) = value_list_to_f64(&e, "ai.search") {
                            out.push(v);
                        } else {
                            return Err("ai.search: failed to parse embedding response".to_string());
                        }
                    }
                    out
                }
                _ => return Err("ai.search: unexpected embedding response shape".to_string()),
            }
        } else {
            // Mock fallback：词袋 + 共享词 hash → 向量
            eprintln!("[ai.search mock — set OPENAI_API_KEY for real semantic search]");
            inputs.iter().map(|s| mock_bow_embedding(s)).collect()
        };

        // 第一个是 query，剩下的对应 corpus
        if embeddings.is_empty() {
            return Err("ai.search: no embeddings returned".to_string());
        }
        let q_vec = &embeddings[0];
        let mut scored: Vec<(usize, f64)> = embeddings[1..]
            .iter()
            .enumerate()
            .map(|(i, v)| (i, cosine_similarity(q_vec, v).unwrap_or(0.0)))
            .collect();
        // 降序
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);

        // 构造 List<Dict{text, score, index}>
        let mut out = Vec::with_capacity(scored.len());
        for (idx, score) in scored {
            let mut m = std::collections::HashMap::new();
            m.insert("text".to_string(), Value::String(corpus[idx].clone()));
            m.insert("score".to_string(), Value::Number(score));
            m.insert("index".to_string(), Value::Number(idx as f64));
            out.push(Value::Dict(m));
        }
        Ok(Value::List(out))
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

    // ===================================================================
    // v11: 向量嵌入 (ai.embed) + 相似度 + 语义检索
    // ===================================================================
    //
    // 设计要点：
    // - 单文本 → List<Number>；批量 (List<String>) → List<List<Number>>
    // - 维度跟随模型（text-embedding-3-small = 1536, v3-large = 3072）
    // - 可选 dimensions 参数（v3 系列支持降维）
    // - 无 API key 时返回错误（沿用 ai.create 策略）
    // - 相似度函数（cosine/dot/euclidean/norm）独立可用，不依赖网络
}

// 实际接收 strings 的版本（避免 self 借用冲突）
fn real_ai_embed_strings(
    inputs: &[String],
    api_key: &str,
    model: &str,
    base_url: &str,
    dim: Option<u32>,
) -> Result<Value, String> {
    if inputs.is_empty() {
        return Err("ai.embed: inputs cannot be empty".to_string());
    }

    // 构造 JSON body
    let escaped_model = model.replace('\\', "\\\\").replace('"', "\\\"");
    let inputs_json: String = inputs.iter()
        .map(|s| {
            let esc = s
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t");
            format!("\"{}\"", esc)
        })
        .collect::<Vec<_>>()
        .join(",");
    let body = if let Some(d) = dim {
        format!(
            r#"{{"model":"{}","input":[{}],"dimensions":{}}}"#,
            escaped_model, inputs_json, d
        )
    } else {
        format!(
            r#"{{"model":"{}","input":[{}]}}"#,
            escaped_model, inputs_json
        )
    };

    let url = format!("{}/embeddings", base_url.trim_end_matches('/'));

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
            Ok(text) => extract_embeddings(&text, inputs.len()),
            Err(e) => Err(format!("ai.embed: failed to read response body: {}", e)),
        },
        Err(ureq::Error::Status(status, response)) => {
            let body = response.into_string().unwrap_or_default();
            let excerpt: String = body.chars().take(300).collect();
            Err(format!(
                "ai.embed: API error HTTP {} from {} (body: {})",
                status, url, excerpt
            ))
        }
        Err(ureq::Error::Transport(t)) => Err(format!(
            "ai.embed: network error connecting to {}: {}",
            url, t
        )),
    }
}

/// 从 OpenAI 兼容 embeddings 响应中提取所有 embedding 向量
///
/// 响应格式: {"data": [{"embedding": [0.1, 0.2, ...], "index": 0}, ...], ...}
/// 返回 Value::List<Vec<Value::Number>> 单条，或 Value::List<Vec<List<...>>> 批量。
fn extract_embeddings(json_text: &str, expected_count: usize) -> Result<Value, String> {
    let root = json_to_value(json_text)?;
    let data = if let Value::Dict(ref map) = root {
        if let Some(Value::List(d)) = map.get("data") {
            d.clone()
        } else {
            return Err("ai.embed: response missing 'data' array".to_string());
        }
    } else {
        return Err("ai.embed: response is not a JSON object".to_string());
    };

    if data.len() != expected_count {
        return Err(format!(
            "ai.embed: expected {} embeddings, got {}",
            expected_count,
            data.len()
        ));
    }

    // 按 index 排序，保证顺序
    let mut indexed: Vec<(usize, Vec<f64>)> = data
        .into_iter()
        .map(|item| {
            if let Value::Dict(m) = item {
                let index = match m.get("index") {
                    Some(Value::Number(n)) => *n as usize,
                    _ => 0,
                };
                let vec = match m.get("embedding") {
                    Some(Value::List(vs)) => vs
                        .iter()
                        .filter_map(|v| if let Value::Number(n) = v { Some(*n) } else { None })
                        .collect(),
                    _ => return Err("ai.embed: 'embedding' field is not a list of numbers".to_string()),
                };
                Ok((index, vec))
            } else {
                Err("ai.embed: data item is not an object".to_string())
            }
        })
        .collect::<Result<Vec<_>, String>>()?;
    indexed.sort_by_key(|(i, _)| *i);

    if expected_count == 1 {
        // 单条：返回一维 List
        let vec = indexed.into_iter().next().unwrap().1;
        Ok(Value::List(vec.into_iter().map(Value::Number).collect()))
    } else {
        // 批量：返回 List<List>
        let items: Vec<Value> = indexed
            .into_iter()
            .map(|(_, v)| Value::List(v.into_iter().map(Value::Number).collect()))
            .collect();
        Ok(Value::List(items))
    }
}

/// 把 Value 列表转成 f64 列表
fn value_list_to_f64(v: &Value, ctx: &str) -> Result<Vec<f64>, String> {
    match v {
        Value::List(items) => items
            .iter()
            .map(|x| match x {
                Value::Number(n) => Ok(*n),
                _ => Err(format!("{}: expected list of numbers", ctx)),
            })
            .collect(),
        _ => Err(format!("{}: expected a list", ctx)),
    }
}

/// 兜底 mock embedding：基于词袋的简单 hash 向量（32 维）
/// 用于 ai.search 在无 API key 时仍能跑通。语义粗糙，但保证端到端 demo 可重现。
fn mock_bow_embedding(s: &str) -> Vec<f64> {
    const DIM: usize = 32;
    let mut v = vec![0.0_f64; DIM];
    for word in s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        let lower = word.to_lowercase();
        // 简单 hash: djb2
        let mut h: u64 = 5381;
        for b in lower.bytes() {
            h = h.wrapping_mul(33).wrapping_add(b as u64);
        }
        v[(h as usize) % DIM] += 1.0;
    }
    v
}

/// 余弦相似度: (a·b) / (||a|| * ||b||)，范围 [-1, 1]
fn cosine_similarity(a: &[f64], b: &[f64]) -> Result<f64, String> {
    if a.len() != b.len() {
        return Err(format!(
            "cosine: vector length mismatch ({} vs {})",
            a.len(),
            b.len()
        ));
    }
    let mut dot = 0.0_f64;
    let mut na = 0.0_f64;
    let mut nb = 0.0_f64;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = na.sqrt() * nb.sqrt();
    if denom == 0.0 {
        return Ok(0.0);
    }
    Ok(dot / denom)
}

/// 点积: a·b
fn dot_product(a: &[f64], b: &[f64]) -> Result<f64, String> {
    if a.len() != b.len() {
        return Err(format!(
            "dot: vector length mismatch ({} vs {})",
            a.len(),
            b.len()
        ));
    }
    Ok(a.iter().zip(b).map(|(x, y)| x * y).sum())
}

/// 欧氏距离: sqrt(sum((a-b)^2))，值越小越相似
fn euclidean_distance(a: &[f64], b: &[f64]) -> Result<f64, String> {
    if a.len() != b.len() {
        return Err(format!(
            "euclidean: vector length mismatch ({} vs {})",
            a.len(),
            b.len()
        ));
    }
    Ok(a.iter().zip(b).map(|(x, y)| (x - y).powi(2)).sum::<f64>().sqrt())
}

/// L2 范数
fn l2_norm(a: &[f64]) -> f64 {
    a.iter().map(|x| x * x).sum::<f64>().sqrt()
}

// ===================================================================
// 控制流信号（v11 重构）
// ===================================================================
//
// 历史：用 `Result<Option<Value>, String>` 同时表达"普通继续"和"return 信号"。
// 这导致 for/if/task 内的 return 无法正确穿透控制流边界。
//
// 重构：用显式 enum 区分两种语义。
// - None: 普通继续，下一条 stmt 正常执行
// - Return(val): return 信号，必须穿透 for/if/try/match 一直冒泡到
//   call_task/call_closure，作为函数返回值
//
// 设计要点：
// - Stmt::Expr 永远返回 None（即使 print 也不携带信号）
// - Stmt::Return 永远返回 Return(val)
// - call_task/call_closure 把 Return(val) 提取出来作为函数返回值；
//   顶层 main 的 Return(val) 被 interpret 静默忽略（Mora 没有 main 返回值概念）
#[derive(Debug, Clone)]
pub enum FlowSignal {
    None,
    Return(Value),
}

impl FlowSignal {
    /// 取出 Return 的值，否则 None 视为 nil（Mora 的"无显式 return"等价于 return nil）
    pub fn into_value(self) -> Value {
        match self {
            FlowSignal::None => Value::Nil,
            FlowSignal::Return(v) => v,
        }
    }

    /// 是 Return 信号吗？
    pub fn is_return(&self) -> bool {
        matches!(self, FlowSignal::Return(_))
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

/// v11: 顶层 read/write 等语句使用的字符串参数提取助手
fn expect_string(value: Value, context: &str) -> Result<String, String> {
    match value {
        Value::String(s) => Ok(s),
        _ => Err(format!("{} must be a string, got {}", context, type_name(&value))),
    }
}

// ===================================================================
// v11: hex 编解码（用于 file.read_bytes / file.write_bytes）
// ===================================================================
//
// 设计要点：
// - 小写字母输出，与 web.fetch / json.* 字符串行为保持一致
// - 输入校验：奇数长度 / 非 hex 字符返回明确错误
// - 性能足够用于 10MB 级别文件，更大文件应考虑 stream API（v11 范围外）
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err(format!("hex length must be even, got {}", s.len()));
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = hex_nibble(bytes[i])
            .ok_or_else(|| format!("invalid hex char '{}' at position {}", bytes[i] as char, i))?;
        let lo = hex_nibble(bytes[i + 1])
            .ok_or_else(|| format!("invalid hex char '{}' at position {}", bytes[i + 1] as char, i + 1))?;
        out.push((hi << 4) | lo);
        i += 2;
    }
    Ok(out)
}

fn hex_nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
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
        Literal::String(s, _) => Value::String(s.clone()),
        Literal::Number(n, _) => Value::Number(*n),
        Literal::Bool(b, _) => Value::Bool(*b),
        Literal::Nil(_) => Value::Nil,
        Literal::List(_, _) => Value::Nil,
        Literal::Dict(_, _) => Value::Nil,
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

// ===================================================================
// v11: 单元测试 — 相似度函数
// ===================================================================
#[cfg(test)]
mod embed_tests {
    use super::*;

    fn approx_eq(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    #[test]
    fn cosine_identical_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        let s = cosine_similarity(&v, &v).unwrap();
        assert!(approx_eq(s, 1.0, 1e-9));
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let s = cosine_similarity(&a, &b).unwrap();
        assert!(approx_eq(s, 0.0, 1e-9));
    }

    #[test]
    fn cosine_opposite_is_minus_one() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        let s = cosine_similarity(&a, &b).unwrap();
        assert!(approx_eq(s, -1.0, 1e-9));
    }

    #[test]
    fn cosine_length_mismatch_errors() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0, 2.0, 3.0];
        assert!(cosine_similarity(&a, &b).is_err());
    }

    #[test]
    fn cosine_zero_vector_safe() {
        let a = vec![0.0, 0.0];
        let b = vec![1.0, 2.0];
        // 分母为 0 应返回 0,不 panic
        let s = cosine_similarity(&a, &b).unwrap();
        assert_eq!(s, 0.0);
    }

    #[test]
    fn dot_product_basic() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        assert_eq!(dot_product(&a, &b).unwrap(), 32.0);  // 4+10+18
    }

    #[test]
    fn euclidean_basic() {
        let a = vec![0.0, 0.0];
        let b = vec![3.0, 4.0];
        let d = euclidean_distance(&a, &b).unwrap();
        assert!(approx_eq(d, 5.0, 1e-9));
    }

    #[test]
    fn norm_unit_vector() {
        let v = vec![3.0, 4.0];
        assert!(approx_eq(l2_norm(&v), 5.0, 1e-9));
    }

    #[test]
    fn mock_bow_same_text_same_vector() {
        // 同一文本两次调用应得到完全相同的向量（确定性）
        let a = mock_bow_embedding("hello world");
        let b = mock_bow_embedding("hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn mock_bow_different_text_different_vector() {
        let a = mock_bow_embedding("alpha beta gamma");
        let b = mock_bow_embedding("xyz foo bar");
        // 32 维中应该至少有几维不同
        let diffs = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        assert!(diffs > 0);
    }
}

// ===================================================================
// 单元测试 — for 循环（修复 v10 之前的 bug：result.is_some() 误判）
// ===================================================================
// 复现主 bug: for body 内任意 Stmt::Expr（如 print）都返回 Some(val)，
// 原代码因此在第一次迭代后中断。修复后用 can_break 闸门，只在
// body 末尾为 Stmt::Return 时才中断。
#[cfg(test)]
mod for_loop_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let stmts = Parser::new(tokens).parse();
        let mut interp = Interpreter::new();
        interp.interpret(&stmts)
    }

    #[test]
    fn for_over_list_runs_all_iters() {
        // 主 bug 复现：原代码 len=3 但只跑 1 次
        let src = r#"
task main()
  let xs = [10, 20, 30]
  let count: number = 0
  for x in xs
    let count = count + 1
  end
  print("count=" + count)
end
"#;
        run(src).expect("for loop should run 3 times");
    }

    #[test]
    fn for_with_print_runs_all_iters() {
        // 关键场景：body 内有 print 副作用。原代码会把 print 返回的
        // Some(Nil) 当成 return 信号，迭代 1 次就停。
        let src = r#"
task main()
  for x in [1, 2, 3]
    print("x=" + x)
  end
end
"#;
        run(src).expect("for with print should run all 3 iterations");
    }

    #[test]
    fn for_over_string_chars() {
        let src = r#"
task main()
  let s = ""
  for c in "abc"
    let s = s + c
  end
  print("s=" + s)
end
"#;
        run(src).expect("for over string should iterate all chars");
    }

    #[test]
    fn for_with_last_stmt_expr_does_not_break() {
        // 显式验证：body 末尾是 Stmt::Expr（如 print）时不中断
        let src = r#"
task main()
  for x in [1, 2, 3, 4, 5]
    print("y=" + (x * 2))
  end
end
"#;
        run(src).expect("for with last stmt expr should not break early");
    }

    #[test]
    fn for_with_last_stmt_let_does_not_break() {
        // 显式验证：body 末尾是 Stmt::Let 时不中断
        let src = r#"
task main()
  for x in [1, 2, 3]
    let y = x * 10
    print("y=" + y)
  end
end
"#;
        run(src).expect("for with last stmt let should not break early");
    }
}

// ===================================================================
// 单元测试 — return 传播（v11 重构）
// ===================================================================
// 修复 4 个 control-flow bug：return 信号原本被 Option<Value> 模糊化，
// 静默丢失在 for/if/try/match 边界。本次用 FlowSignal enum 显式区分
// None / Return(val)，并验证信号穿透所有控制结构。
#[cfg(test)]
mod return_propagation_tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn run(src: &str) -> Result<(), String> {
        let tokens = Lexer::new(src).scan_tokens();
        let stmts = Parser::new(tokens).parse();
        let mut interp = Interpreter::new();
        interp.interpret(&stmts)
    }

    #[test]
    fn return_in_for_propagates_to_task() {
        // for body 内的 return 必须穿透 for 边界到外层 task，作为函数返回值
        let src = r#"
task main()
  task find(xs: list, t: number)
    for x in xs
      if x == t then
        return x
      end
    end
    return -1
  end
  let _ = find([1, 2, 3], 3)
end
"#;
        run(src).expect("return in for should propagate");
    }

    #[test]
    fn return_in_if_propagates_to_task() {
        let src = r#"
task main()
  task check(x: number)
    if x > 5 then
      return "big"
    end
    return "small"
  end
  let _ = check(10)
end
"#;
        run(src).expect("return in if should propagate");
    }

    #[test]
    fn return_in_try_does_not_trigger_catch() {
        // try 块内 return 不应进 catch；应当穿透 try 边界向外冒泡
        let src = r#"
task main()
  task maybe(blow: bool)
    try
      if blow then
        return 42
      end
      return 100
    catch err
      return -1
    end
  end
  let _ = maybe(true)
end
"#;
        run(src).expect("return in try should not trigger catch");
    }

    #[test]
    fn return_continues_after_loop() {
        // for 跑完所有迭代（无 return）后，task 继续往下执行
        let src = r#"
task main()
  task count()
    let total: number = 0
    for x in [1, 2, 3]
      let total = total + x
    end
    return total
  end
  let _ = count()
end
"#;
        run(src).expect("should continue after loop");
    }

    #[test]
    fn closure_expression_returns_value() {
        // fn(x) x * 2 end 的闭包返回值是 x*2，不是 nil
        // （这是闭包语义，不是 task 语义——闭包 body 单 expr 自动是返回值）
        let src = r#"
task main()
  let f = fn(x) x * 2 end
  let _ = f(5)
end
"#;
        run(src).expect("closure expression should return value");
    }

    #[test]
    fn flow_signal_into_value_handles_none() {
        // FlowSignal::None → nil (Mora 的"无显式 return"语义)
        assert_eq!(FlowSignal::None.into_value(), Value::Nil);
        assert_eq!(
            FlowSignal::Return(Value::Number(42.0)).into_value(),
            Value::Number(42.0)
        );
    }

    #[test]
    fn flow_signal_is_return_distinguishes_signals() {
        assert!(!FlowSignal::None.is_return());
        assert!(FlowSignal::Return(Value::Nil).is_return());
        assert!(FlowSignal::Return(Value::Number(0.0)).is_return());
    }
}
