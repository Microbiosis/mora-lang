//! v0.92 P1.2: `Interpreter::call_builtin_*` — builtin 函数实现（自 dispatch.rs 迁出）。
//!
//! 按 concern 拆分：dispatch.rs 只保留路由（`call_function` 的 name→handler
//! 分派 + `call_value` 的 Value→调用），本模块承载 30 个 `call_builtin_*`
//! 具体实现 + 兜底查找。`testcase!` 宏定义在 `interpreter` 模块根。

use parking_lot::Mutex;

use super::*;
use crate::value::Value;

impl Interpreter {
    pub(super) fn call_builtin_merge_with(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let key = match args.first() {
            Some(Value::String(s)) => {
                testcase!(true, "merge_with: string key");
                s.clone()
            }
            _ => return Err("merge_with(key, strategy) expects string key".to_string()),
        };
        let strat = match args.get(1) {
            Some(Value::String(s)) => {
                testcase!(true, "merge_with: string strategy");
                s.as_str()
            }
            _ => {
                return Err("merge_with(key, strategy) expects string strategy".to_string());
            }
        };
        // v0.75.24: 策略名解析收敛到 MergeStrategy::from_name
        // （单一事实来源；typeck 对字面量参数已做编译期校验，此处
        // 运行时兜底未类型化用法）。
        let ms = match crate::value::MergeStrategy::from_name(strat) {
            Some(s) => s,
            None => {
                return Err(format!(
                    "merge_with: unknown strategy '{}' (append/add/dict_union/grow_only_set/lww)",
                    strat
                ));
            }
        };
        let mut strategies = self.current_merge_strategies().unwrap_or_default();
        strategies.insert(key, ms);
        self.set_merge_strategies(Some(strategies));
        Ok(Value::Nil)
    }

    pub(super) fn call_builtin_print(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let msg = args
            .into_iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\t");
        println!("{}", msg);
        Ok(Value::Nil)
    }

    pub(super) fn call_builtin_range(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let start = args
            .first()
            .and_then(|v| match v {
                Value::Float(n) => Some(*n as i64),
                _ => None,
            })
            .unwrap_or(0);
        let end = args
            .get(1)
            .and_then(|v| match v {
                Value::Float(n) => Some(*n as i64),
                _ => None,
            })
            .unwrap_or(start);
        let step = args
            .get(2)
            .and_then(|v| match v {
                Value::Float(n) => Some(*n as i64),
                _ => None,
            })
            .unwrap_or(1);
        let mut items = Vec::new();
        let mut i = start;
        while i < end {
            items.push(Value::Float(i as f64));
            i += step;
        }
        Ok(Value::List(items))
    }

    pub(super) fn call_builtin_len(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let len = match args.first() {
            Some(Value::List(list)) => {
                testcase!(true, "len: list");
                list.len()
            }
            Some(Value::String(s)) => {
                testcase!(true, "len: string");
                s.len()
            }
            Some(Value::Dict(map)) => {
                testcase!(true, "len: dict");
                map.len()
            }
            _ => return Err("len() expects a list, string, or dict".to_string()),
        };
        Ok(Value::Int(len as i64))
    }

    pub(super) fn call_builtin_compose(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("compose() requires at least 1 argument".to_string());
        }
        // 返回一个特殊的 Compose 值
        Ok(Value::Compose(args))
    }

    pub(super) fn call_builtin_partial(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("partial() requires at least 1 argument (the function)".to_string());
        }
        let func = args[0].clone();
        let partial_args: Vec<Value> = args[1..].to_vec();
        Ok(Value::Partial(Box::new(func), partial_args))
    }

    pub(super) fn call_builtin_atom(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let value = args.first().cloned().unwrap_or(Value::Nil);
        Ok(Value::Atom(Arc::new(Mutex::new(value))))
    }

    pub(super) fn call_builtin_swap(
        &mut self,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("swap() requires 2 arguments: atom and function".to_string());
        }
        match &args[0] {
            Value::Atom(arc) => {
                let func = &args[1];
                let old = arc.lock().clone();
                let new_val = self.call_value(func, vec![old], effects)?;
                *arc.lock() = new_val.clone();
                Ok(new_val)
            }
            _ => Err("swap() first argument must be an atom".to_string()),
        }
    }

    pub(super) fn call_builtin_deref(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let value = args.first().ok_or("deref() requires 1 argument")?;
        match value {
            Value::Atom(arc) => Ok(arc.lock().clone()),
            _ => Err("deref() argument must be an atom".to_string()),
        }
    }

    pub(super) fn call_builtin_type_of(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let value = args.first().ok_or("type_of() requires 1 argument")?;
        Ok(Value::String(value_type_name(value).to_string()))
    }

    pub(super) fn call_builtin_is_instance(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("is_instance() requires 2 arguments".to_string());
        }
        let value = &args[0];
        let type_name = match &args[1] {
            Value::String(s) => s.as_str(),
            _ => return Err("is_instance() second argument must be a string".to_string()),
        };
        Ok(Value::Bool(value_type_name(value) == type_name))
    }

    pub(super) fn call_builtin_methods_of(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let value = args.first().ok_or("methods_of() requires 1 argument")?;
        let methods = value.methods();
        Ok(Value::List(
            methods.into_iter().map(Value::String).collect(),
        ))
    }

    pub(super) fn call_builtin_compress(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("compress() requires 2 arguments: input and strategy".to_string());
        }
        let strategy = match &args[1] {
            Value::String(s) => s.clone(),
            other => {
                return Err(format!(
                    "compress: strategy must be a string, got {:?}",
                    other
                ));
            }
        };
        let options_val = args
            .get(2)
            .cloned()
            .unwrap_or(Value::Dict(Default::default()));
        let opts_base =
            crate::compress::options_from_value(&options_val).map_err(|e| e.to_string())?; // v0.75.99: MoraError → String 转换
        let opts = crate::compress::CompressOptions {
            strategy: strategy.clone(),
            ..opts_base
        };
        crate::compress::compress_top(&args[0], &strategy, &opts).map_err(|e| e.to_string())
    }

    pub(super) fn call_builtin_crush_json(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("crush_json() requires 2 arguments: input and max".to_string());
        }
        let max_items = match &args[1] {
            Value::Float(n) => {
                if *n < 0.0 {
                    return Err("crush_json: max must be non-negative".to_string());
                }
                *n as usize
            }
            other => {
                return Err(format!("crush_json: max must be a number, got {:?}", other));
            }
        };
        let options_val = args
            .get(2)
            .cloned()
            .unwrap_or(Value::Dict(Default::default()));
        let opts = crate::compress::options_from_value(&options_val).map_err(|e| e.to_string())?; // v0.75.99: MoraError → String 转换
        let items = match &args[0] {
            Value::List(l) => l.clone(),
            _ => {
                return Err("crush_json: expected List as first argument".to_string());
            }
        };
        let result = crate::compress::crush_json(&items, max_items, &opts);
        let json = crate::compress::value_to_json_simple(&Value::List(result.items.clone()));
        Ok(Value::String(format!(
            "{}\n<compressed:method=smart_crusher strategy={} items={} total={} savings={:.2}>",
            json, result.strategy_used, result.items_kept, result.items_total, result.savings_ratio
        )))
    }

    pub(super) fn call_builtin_batch_chat(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let prompts = args
            .first()
            .ok_or("batch_chat() requires 1 argument (list of prompts)")?;
        match prompts {
            Value::List(items) => {
                let mut results = Vec::new();
                for item in items {
                    let prompt = match item {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let model = std::env::var(AI_MODEL_ENV)
                        .unwrap_or_else(|_| AI_MODEL_DEFAULT.to_string());
                    let result = Self::do_ai_chat(self, &model, &prompt)?;
                    results.push(result);
                }
                Ok(Value::List(results))
            }
            _ => Err("batch_chat() argument must be a list".to_string()),
        }
    }

    pub(super) fn call_builtin_into(
        &mut self,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("into() requires 2 arguments: collection and function".to_string());
        }
        let collection = args[0].clone();
        let transform = args[1].clone();
        match collection {
            Value::List(list) => {
                let mut result = Vec::new();
                for item in list {
                    let mapped = self.call_value(&transform, vec![item], effects)?;
                    match mapped {
                        Value::List(items) => result.extend(items),
                        other => result.push(other),
                    }
                }
                Ok(Value::List(result))
            }
            _ => Err("into() first argument must be a list".to_string()),
        }
    }

    pub(super) fn call_builtin_tail(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("tail() requires 2 arguments: path and max".to_string());
        }
        let path = match &args[0] {
            Value::String(s) => s.clone(),
            other => {
                return Err(format!(
                    "tail() first argument must be a string path, got {:?}",
                    other
                ));
            }
        };
        let max: usize = match &args[1] {
            Value::Float(n) => {
                if *n < 0.0 {
                    return Err("tail() max must be non-negative".to_string());
                }
                *n as usize
            }
            _ => return Err("tail() second argument 'max' must be a number".to_string()),
        };
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("tail() cannot read '{}': {}", path, e))?;
        let lines: Vec<&str> = content.lines().collect();
        let start = if lines.len() > max {
            lines.len() - max
        } else {
            0
        };
        let tail_str = lines[start..].join("\n");
        Ok(Value::String(tail_str))
    }

    /// v0.103 修复：section 从**执行环境**解析（与 `h_prompt_section` 写入的
    /// 环境同一处）。此前读 `self.core.environment` —— 而 `take_env` 已把
    /// 宿主的该字段取空交给执行路径，故 `prompt "x" do ... end` 定义的
    /// section 永远查不到（报 "section 'x' not defined"，错误消息还指引
    /// 用户写当时 parser 不产出的语法）。与 eval/macroexpand 同一 env 穿线模式。
    pub(super) fn call_builtin_compose_prompt(
        &mut self,
        args: Vec<Value>,
        env: &Environment,
    ) -> Result<Value, String> {
        if args.is_empty() {
            return Err("compose_prompt() requires at least 1 section".to_string());
        }
        let mut buf = String::new();
        for arg in args {
            let (name, role, text, budget_bytes) = match arg {
                Value::String(section_name) => {
                    // 从环境查 section（v0.95: 纯值只读查询，无锁）
                    let looked_up = env.get(&section_name);
                    match looked_up {
                        Some(Value::PromptSection {
                            name,
                            role,
                            text,
                            budget_bytes,
                        }) => (name, role, text, budget_bytes),
                        Some(other) => {
                            return Err(format!(
                                "compose_prompt: '{}' is not a prompt section (got {:?})",
                                section_name, other
                            ));
                        }
                        None => {
                            return Err(format!(
                                "compose_prompt: section '{}' not defined (use 'prompt \"{}\" do ... end' first)",
                                section_name, section_name
                            ));
                        }
                    }
                }
                Value::Dict(map) => {
                    let role = map.get("role").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    });
                    let text_val = map
                        .get("text")
                        .cloned()
                        .unwrap_or(Value::String(String::new()));
                    let budget = if let Some(b) = map.get("budget") {
                        Some(super::numeric_helpers::parse_budget_dispatch(b.clone(), "budget")?)
                    } else {
                        None
                    };
                    ("<inline>".to_string(), role, Box::new(text_val), budget)
                }
                Value::PromptSection {
                    name,
                    role,
                    text,
                    budget_bytes,
                } => (name, role, text, budget_bytes),
                other => {
                    return Err(format!(
                        "compose_prompt: section must be name, dict, or PromptSection (got {:?})",
                        other
                    ));
                }
            };
            // 应用 budget 截断
            let resolved_text = super::numeric_helpers::text_to_string(&text);
            let truncated = match budget_bytes {
                Some(b) if resolved_text.len() > b => {
                    let mut t = resolved_text.into_bytes();
                    t.truncate(b);
                    String::from_utf8_lossy(&t).into_owned()
                }
                _ => resolved_text,
            };
            // 拼接
            if let Some(r) = &role {
                buf.push_str(&format!("\n## {} ({})\n\n", name, r));
            } else {
                buf.push_str(&format!("\n## {}\n\n", name));
            }
            buf.push_str(&truncated);
        }
        Ok(Value::String(buf))
    }
    pub(super) fn call_builtin_eval(
        &mut self,
        args: Vec<Value>,
        env: &Environment,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        // v0.86: runtime eval — 从 Mora 代码内部动态执行任意 Mora 源码。
        // 这是 Lisp homoiconicity + eval-apply loop 在 Mora 上的落地：
        //   eval("2 + 3") -> Value::Int(5)
        //   eval("function foo(x) x*x end") -> Value::Task/Closure
        // 实现路径：source string -> Lexer -> ParserV3::compile -> run_mir。
        // 执行环境：子 Environment 以当前调用栈的 env 为 parent（用 parking_lot::Mutex 与
        // value.rs 中的 Environment 类型一致），这样 eval 内部可以读取当前 scope
        // 的变量（如 task 内的 let 绑定），但无法修改外层绑定。
        let source = match args.first() {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Code(s)) => s.clone(),
            _ => {
                return Err("eval(source: string|code) expects a string or code argument".to_string());
            }
        };
        let (func, _witnesses) = match crate::parser_v3::ParserV3::compile(&source) {
            Ok(f) => f,
            Err(e) => return Err(format!("eval: compile error: {}", e)),
        };
        let func = std::sync::Arc::new(func);
        let mut child_env = Environment::with_parent_of(std::sync::Arc::new(env.clone()));
        crate::mir::vm::run_mir(&func, self, &mut child_env, effects)
    }

    // ===================================================================
    // v0.86: Lisp homoiconicity — apply / curry / uncurry / cons / car / cdr
    // ===================================================================

    /// v0.86: apply(fn, [args...]) — Lisp apply。将 args 列表展开为独立参数
    /// 并调用 fn。这是 eval-apply loop 的另一半（eval 已完成）。
    /// 示例：apply(sum, [1, 2, 3]) == sum(1, 2, 3)
    pub(super) fn call_builtin_apply(
        &mut self,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("apply(fn, [args...]) expects at least 2 arguments".to_string());
        }
        let fn_val = &args[0];
        let arg_list = &args[1];
        // 提取待展开的参数列表
        let expanded: Vec<Value> = match arg_list {
            Value::List(items) => items.clone(),
            // 单个元素也算作一个参数的"列表"
            other => vec![other.clone()],
        };
        self.call_value(fn_val, expanded, effects)
    }

    /// v0.86: curry(fn, arity) — 柯里化包装器。
    /// 返回 Value::Curry(func, arity, bound_args=[])。
    /// 调用时若参数数 < arity，返回新的 Curry（累积参数）；
    /// 若参数数 >= arity，调用内部 fn。
    /// 示例：
    ///   let f = curry(sum3, 3)
    ///   f(1)(2)(3) -> 6
    ///   curry(add, 2)(1)(2) -> 3
    pub(super) fn call_builtin_curry(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("curry(fn, arity) expects fn and arity".to_string());
        }
        let fn_val = args[0].clone();
        let arity = match &args[1] {
            Value::Int(n) => *n as usize,
            Value::Float(n) => *n as usize,
            _ => return Err("curry: arity must be an integer".to_string()),
        };
        if arity == 0 {
            return Err("curry: arity must be > 0".to_string());
        }
        Ok(Value::Curry {
            func: Box::new(fn_val),
            arity,
            bound_args: Vec::new(),
        })
    }

    /// v0.86: uncurry(curried_fn) — 解柯里化。
    /// 若参数是 Curry，返回内部原始 fn；否则原样返回。
    pub(super) fn call_builtin_uncurry(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("uncurry(fn) expects a function argument".to_string());
        }
        match &args[0] {
            Value::Curry { func, .. } => Ok((**func).clone()),
            _ => Ok(args[0].clone()),
        }
    }

    /// v0.86: cons(car, cdr) — Lisp cons cell 构造器。
    /// 返回 Value::Cons(car, cdr)。
    /// 示例：cons(1, cons(2, Nil)) -> (1 . (2))
    pub(super) fn call_builtin_cons(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("cons(car, cdr) expects 2 arguments".to_string());
        }
        Ok(Value::Cons {
            car: Box::new(args[0].clone()),
            cdr: Box::new(args[1].clone()),
        })
    }

    /// v0.86: car(cell) — 提取 cons cell 的头部。
    /// 对 List 退化为取第一个元素，方便过渡。
    pub(super) fn call_builtin_car(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("car(cell) expects a cons cell or list".to_string());
        }
        match &args[0] {
            Value::Cons { car, .. } => Ok((**car).clone()),
            Value::List(items) => items
                .first()
                .cloned()
                .ok_or_else(|| "car: empty list has no first element".to_string()),
            Value::Nil => Err("car: cannot take car of Nil".to_string()),
            _ => Err(format!("car: expected cons cell or list, got {}", args[0])),
        }
    }

    /// v0.86: cdr(cell) — 提取 cons cell 的尾部。
    /// 对 List 退化为取除第一个元素外的剩余列表。
    pub(super) fn call_builtin_cdr(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("cdr(cell) expects a cons cell or list".to_string());
        }
        match &args[0] {
            Value::Cons { cdr, .. } => Ok((**cdr).clone()),
            Value::List(items) => Ok(Value::List(items[1..].to_vec())),
            Value::Nil => Err("cdr: cannot take cdr of Nil".to_string()),
            _ => Err(format!("cdr: expected cons cell or list, got {}", args[0])),
        }
    }

    /// v0.86: quote(s) — Lisp homoiconicity 的第三块基石。
    ///
    /// 将源码文本字符串包装为 Value::Code，与 eval 形成往返对：
    ///   eval(quote(expr)) == expr
    ///   quote(expr) -> Value::Code("expr")
    ///
    /// 注意：`quote(expr)` 语法在解析期已由 emit_quote_w 提取源码文本
    /// 并以 MirInst::Call(dst, "quote", [Const(String(expr))]) 的形式 emit。
    /// 本函数是运行时入口：把 Value::String 转换为 Value::Code。
    pub(super) fn call_builtin_quote(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let source = match args.first() {
            Some(Value::String(s)) => s.clone(),
            _ => {
                return Err(
                    "quote(source: string) expects a string argument".to_string(),
                );
            }
        };
        Ok(Value::Code(source))
    }

    // ===================================================================
    // v0.87: Lisp homoiconicity 三件套补完 — gensym / read / macroexpand
    // ===================================================================

    /// v0.87: gensym() — 生成唯一符号名。
    ///
    /// Lisp 宏系统的核心原语：保证宏展开时引入的新变量名不与用户代码冲突。
    /// Mora 无 AST 架构下，gensym 返回一个 `Value::String`，形式为 `"g{n}"`，
    /// 保证同一 Interpreter 实例内唯一（gensym_counter 在 CoreRuntime 上，
    /// v0.95 起是纯 `usize`，`&mut self` 递增；Pregel worker 各自独立）。
    ///
    /// 用法：
    ///   let fresh = gensym()   → "g0"
    ///   let fresh2 = gensym()  → "g1"
    ///
    pub(super) fn call_builtin_gensym(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if !args.is_empty() {
            return Err("gensym() expects no arguments".to_string());
        }
        let n = self.core.gensym_counter;
        self.core.gensym_counter += 1;
        Ok(Value::String(format!("g{n}")))
    }

    /// v0.87: read(code_str) — 解析源码字符串为 Value::Code。
    ///
    /// 与 quote(expr) 语义等价（Mora 无 AST，两者都是把源码文本作为可执行代码载体）。
    /// read 是 Lisp 传统的 Reader 层函数（字符流 → 数据），在 Mora 中简化为
    /// "字符串 → Value::Code" 标记。与 eval() 配对使用：eval(read(code_str)) 等价于 eval(code_str)。
    ///
    /// 用法：
    ///   let code = read("2 + 3")     → Value::Code("2 + 3")
    ///   eval(read("2 + 3"))          → Value::Int(5)
    ///   eval(read(read("2 + 3")))    → 嵌套：read 返回 Code，eval 接受 Code
    ///
    pub(super) fn call_builtin_read(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let source = match args.first() {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Code(s)) => s.clone(),
            _ => {
                return Err("read(source: string|code) expects a string or code argument".to_string());
            }
        };
        Ok(Value::Code(source))
    }

    /// v0.87: macroexpand(name, args...) — 展开宏并返回求值结果。
    ///
    /// Mora 宏是 by-example（运行时求值）而非 compile-time source 变换。
    /// macroexpand 执行宏 body（以 args 绑定 params），返回结果作为"展开值"。
    /// 这对应 CL 中 macroexpand 求值展开形式的语义。
    ///
    /// 用法：
    ///   macro add(a, b)  a + b  end
    ///   macroexpand("add", [1, 2])   → Value::Int(3)
    ///
    ///   macro when(cond, body)  if cond { body } end
    ///   macroexpand("when", [true, 42])  → Value::Int(42)
    ///
    /// 错误路径：
    ///   - 未定义宏 → "macroexpand: undefined macro 'name'"
    ///   - 参数不足 → 缺失参数绑定为 Nil
    ///   - 宏执行错误 → "macro 'name' execution error: ..."
    ///
    pub(super) fn call_builtin_macroexpand(
        &mut self,
        args: Vec<Value>,
        env: &Environment,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        let name = match args.first() {
            Some(Value::String(s)) => s.clone(),
            _ => {
                return Err(
                    "macroexpand(name: string, args...) expects a string name".to_string(),
                );
            }
        };
        let expr_args: Vec<Value> = if args.len() > 1 {
            match &args[1] {
                Value::List(items) => items.clone(),
                other => vec![other.clone()],
            }
        } else {
            Vec::new()
        };

        let macro_val = env
            .get(&name)
            .ok_or_else(|| format!("macroexpand: undefined macro '{}'", name))?;

        match macro_val {
            Value::Macro {
                name: mname,
                params,
                body,
            } => {
                let mut child_env =
                    Environment::with_parent_of(std::sync::Arc::new(env.clone()));
                for (i, param) in params.iter().enumerate() {
                    let val = expr_args.get(i).cloned().unwrap_or(Value::Nil);
                    child_env.define(param.clone(), val, false);
                }
                crate::mir::vm::run_mir(&body, self, &mut child_env, effects).map_err(|e| {
                    format!("macro '{}' expansion error: {}", mname, e)
                })
            }
            _ => Err(format!("macroexpand: '{}' is not a macro", name)),
        }
    }

    pub(super) fn call_builtin_fallback(
        &mut self,
        name: &str,
        args: Vec<Value>,
        env: &Environment,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        let looked_up = env.get(name).clone();
        if let Some(value) = looked_up {
            match value {
                Value::Task { .. }
                | Value::Closure { .. }
                | Value::Compose(_)
                | Value::Partial(_, _)
                | Value::Curry { .. } => self.call_value(&value, args, effects),
                // v0.83: 宏展开 — 完整实现。宏体是 MIR 函数（由 parser emit_macro_def_w
                // 经子 EmitContext 编译而来），调用时以 args 绑定 params，在子 env 中
                // run_mir 执行 body（与 Value::Task 语义同构）。
                Value::Macro {
                    name: mname,
                    params,
                    body,
                } => {
                    // v0.86: 宏体执行的 parent 环境改为当前调用栈 env（而非全局环境），
                    // 这样宏可以引用同级定义的其他宏和变量（如 square 调 mul）。
                    let mut child_env =
                        Environment::with_parent_of(std::sync::Arc::new(env.clone()));
                    for (i, param) in params.iter().enumerate() {
                        let val = args.get(i).cloned().unwrap_or(Value::Nil);
                        child_env.define(param.clone(), val, false);
                    }
                    crate::mir::vm::run_mir(&body, self, &mut child_env, effects).map_err(|e| {
                        format!("macro '{}' execution error: {}", mname, e)
                    })
                }
                _ => Err(format!("'{}' is not callable", name)),
            }
        } else {
            Err(format!("Undefined function or task: {}", name))
        }
    }
}
