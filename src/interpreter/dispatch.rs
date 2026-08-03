//! 函数分发模块

use super::*;
use crate::common::Span;
use crate::value::{BuiltinKind, Value};

/// v0.75.49: `testcase!(cond, label)` — SQLite `testcase()` 同款断言宏
/// （D1 借石：VDBE 用它在分支守卫处插桩，标注「此分支有专门测试用例」）。
///
/// 语义：开发版（debug_assertions）断言 `cond` 为真并携带分支名 —— 守卫
/// 若被意外绕过（分支语义漂移）立即暴露；release 版零开销（空转）。
/// 用法：插在 builtin 类型守卫分支的命中处，把「分支可达性」从裸 match
/// 显式化为可 grep、可审计的标注 —— 覆盖意图自文档化，为 P6
/// （BuiltinId 静态表）铺路。
macro_rules! testcase {
    ($cond:expr, $label:expr) => {
        debug_assert!($cond, "testcase: {} — 分支守卫被意外绕过", $label)
    };
}

/// S8 fix: 安全地在 async runtime 上阻塞执行，避免嵌套 panic。
///
/// `Runtime::new().unwrap().block_on()` 在已处于 tokio context 时会 panic
/// ("Cannot start a runtime from within a runtime")。本 helper 先检测当前
/// 是否已有 runtime handle：有则用 `block_in_place` + handle.block_on（要求
/// multi-threaded runtime，mora 的 http/mcp server 默认用 rt-multi-thread），
/// 无则新建 Runtime。同时消除 `.unwrap()` panic 风险。
fn block_on_async<F: std::future::Future>(future: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            // 边缘情况：HTTP/MCP handler 内的 Mora 代码又调 Router.listen / McpServer.serve
            tokio::task::block_in_place(|| handle.block_on(future))
        }
        Err(_) => {
            // 常态：从 sync 解释器调用，不在 runtime 内
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("failed to create tokio runtime for serve")
                .block_on(future)
        }
    }
}

impl Interpreter {
    pub(super) fn call_function(
        &mut self,
        name: &str,
        args: Vec<Value>,
        env: &Environment,
        call_site: Span,
    ) -> Result<Value, String> {
        // v0.08.2: Trait::new("ForType") —— 构造 trait instance
        //   data = {"_type": "ForType"}，vtable 绑定所有 impl methods
        // v0.09: 支持 `Trait<T>::new("ForType")` 解析 generics
        if let Some(tname) = name.strip_suffix("::new") {
            // v0.09: 解析 tname 中的 `<...>` 泛型（namespace 已经拼成 "Foo<T,U>"）
            let (trait_name, trait_generics) = if let Some(lt) = tname.find('<') {
                let n = &tname[..lt];
                let gens_str = &tname[lt + 1..tname.len() - 1];
                let gens: Vec<String> = if gens_str.is_empty() {
                    vec![]
                } else {
                    gens_str.split(',').map(|s| s.trim().to_string()).collect()
                };
                (n.to_string(), gens)
            } else {
                (tname.to_string(), vec![])
            };
            if self.registry.trait_registry.contains_key(&trait_name) {
                let type_arg = args.first().map(|v| v.to_string()).unwrap_or_default();
                return self.construct_trait_instance(
                    &trait_name,
                    &trait_generics,
                    &type_arg,
                    &[],
                    call_site,
                );
            }
        }
        // v0.75.52: P6 — BuiltinKind 静态表登记校验（校验点已移至 `_` 兜底
        // 分支，v0.75.76：顶层断言误拦用户自定义函数）。from_name 是 name→kind
        // 的单一来源；此处仅取 kind 供兜底分支判定。
        let _kind = crate::value::BuiltinKind::from_name(name);
        match name {
            "merge_with" => self.call_builtin_merge_with(args),
            "print" => self.call_builtin_print(args),
            "range" => self.call_builtin_range(args),
            "len" => self.call_builtin_len(args),
            "compose" => self.call_builtin_compose(args),
            "partial" => self.call_builtin_partial(args),
            "atom" => self.call_builtin_atom(args),
            "swap" => self.call_builtin_swap(args),
            "deref" => self.call_builtin_deref(args),
            "type_of" => self.call_builtin_type_of(args),
            "is_instance" => self.call_builtin_is_instance(args),
            "methods_of" => self.call_builtin_methods_of(args),
            "compress" => self.call_builtin_compress(args),
            "crush_json" => self.call_builtin_crush_json(args),
            "batch_chat" => self.call_builtin_batch_chat(args),
            "into" => self.call_builtin_into(args),
            "tail" => self.call_builtin_tail(args),
            "compose_prompt" => self.call_builtin_compose_prompt(args),
            _ => {
                // v0.75.76: P6 登记校验移至兜底分支——此前顶层 testcase! 断言
                // `_kind.is_some()` 误拦用户自定义函数（_kind.is_none() 落兜底
                // 环境查找），实际运行 `let f = fn(x) x*2 end; f(1)` 即 panic。
                // 正确语义：builtin 名不得落兜底（登记与 match 漂移），
                // 非 builtin（用户函数/哨兵/merge）合法落兜底。
                testcase!(
                    _kind.is_none() || name.starts_with("__") || matches!(name, "merge_with"),
                    format!(
                        "call_function: builtin 名 {name} 落入兜底（BuiltinKind::from_name 登记与 match 不一致）"
                    )
                );
                self.call_builtin_fallback(name, args, env)
            }
        }
    }

    fn call_builtin_merge_with(&mut self, args: Vec<Value>) -> Result<Value, String> {
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
        // 运行时兜底动态用法）。
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

    fn call_builtin_print(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let msg = args
            .into_iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\t");
        println!("{}", msg);
        Ok(Value::Nil)
    }

    fn call_builtin_range(&mut self, args: Vec<Value>) -> Result<Value, String> {
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

    fn call_builtin_len(&mut self, args: Vec<Value>) -> Result<Value, String> {
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

    fn call_builtin_compose(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("compose() requires at least 1 argument".to_string());
        }
        // 返回一个特殊的 Compose 值
        Ok(Value::Compose(args))
    }

    fn call_builtin_partial(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("partial() requires at least 1 argument (the function)".to_string());
        }
        let func = args[0].clone();
        let partial_args: Vec<Value> = args[1..].to_vec();
        Ok(Value::Partial(Box::new(func), partial_args))
    }

    fn call_builtin_atom(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let value = args.first().cloned().unwrap_or(Value::Nil);
        Ok(Value::Atom(Arc::new(Mutex::new(value))))
    }

    fn call_builtin_swap(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("swap() requires 2 arguments: atom and function".to_string());
        }
        match &args[0] {
            Value::Atom(arc) => {
                let func = &args[1];
                let old = arc.lock().clone();
                let new_val = self.call_value(func, vec![old])?;
                *arc.lock() = new_val.clone();
                Ok(new_val)
            }
            _ => Err("swap() first argument must be an atom".to_string()),
        }
    }

    fn call_builtin_deref(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let value = args.first().ok_or("deref() requires 1 argument")?;
        match value {
            Value::Atom(arc) => Ok(arc.lock().clone()),
            _ => Err("deref() argument must be an atom".to_string()),
        }
    }

    fn call_builtin_type_of(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let value = args.first().ok_or("type_of() requires 1 argument")?;
        Ok(Value::String(value_type_name(value).to_string()))
    }

    fn call_builtin_is_instance(&mut self, args: Vec<Value>) -> Result<Value, String> {
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

    fn call_builtin_methods_of(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let value = args.first().ok_or("methods_of() requires 1 argument")?;
        let methods = value.methods();
        Ok(Value::List(
            methods.into_iter().map(Value::String).collect(),
        ))
    }

    fn call_builtin_compress(&mut self, args: Vec<Value>) -> Result<Value, String> {
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
        let opts_base = crate::compress::options_from_value(&options_val)?;
        let opts = crate::compress::CompressOptions {
            strategy: strategy.clone(),
            ..opts_base
        };
        crate::compress::compress_top(&args[0], &strategy, &opts)
    }

    fn call_builtin_crush_json(&mut self, args: Vec<Value>) -> Result<Value, String> {
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
        let opts = crate::compress::options_from_value(&options_val)?;
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

    fn call_builtin_batch_chat(&mut self, args: Vec<Value>) -> Result<Value, String> {
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

    fn call_builtin_into(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.len() < 2 {
            return Err("into() requires 2 arguments: collection and function".to_string());
        }
        let collection = args[0].clone();
        let transform = args[1].clone();
        match collection {
            Value::List(list) => {
                let mut result = Vec::new();
                for item in list {
                    let mapped = self.call_value(&transform, vec![item])?;
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

    fn call_builtin_tail(&mut self, args: Vec<Value>) -> Result<Value, String> {
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

    fn call_builtin_compose_prompt(&mut self, args: Vec<Value>) -> Result<Value, String> {
        if args.is_empty() {
            return Err("compose_prompt() requires at least 1 section".to_string());
        }
        let mut buf = String::new();
        for arg in args {
            let (name, role, text, budget_bytes) = match arg {
                Value::String(section_name) => {
                    // 从环境查 section
                    let looked_up = self.core.environment.lock().get(&section_name);
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
                        Some(parse_budget_dispatch(b.clone(), "budget")?)
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
            let resolved_text = text_to_string(&text);
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
    fn call_builtin_fallback(
        &mut self,
        name: &str,
        args: Vec<Value>,
        env: &Environment,
    ) -> Result<Value, String> {
        // v0.75.76: 用户函数查找源为执行 env（经参数单一传递，与 h_define
        // 同一容器）——不再查询宿主全局环境（take_env 空壳问题根除）。
        let looked_up = env.get(name).clone();
        if let Some(value) = looked_up {
            match value {
                Value::Task { .. }
                | Value::Closure { .. }
                | Value::Compose(_)
                | Value::Partial(_, _) => self.call_value(&value, args),
                Value::Macro { params, .. } => {
                    let env = Arc::new(Mutex::new(Environment::with_parent_of(
                        self.core.environment.clone(),
                    )));
                    for (i, param) in params.iter().enumerate() {
                        let value = args.get(i).cloned().unwrap_or(Value::Nil);
                        env.lock().define(param.clone(), value, false);
                    }
                    // Macro body 在 v2 模式下通过 arena 执行，此处简化返回 Nil
                    Ok(Value::Nil)
                }
                _ => Err(format!("'{}' is not callable", name)),
            }
        } else {
            Err(format!("Undefined function or task: {}", name))
        }
    }

    /// v0.17: 直接调用 Value 形式的函数（用于管道闭包）
    #[allow(dead_code)]
    pub(super) fn call_method(
        &mut self,
        object: Value,
        method: &str,
        args: Vec<Value>,
        call_site: Span,
    ) -> Result<Value, String> {
        // v0.35: inline-cache 已删；TraitObject 走 dispatch_trait_method
        if let Value::TraitObject { .. } = &object {
            return self.dispatch_trait_method(&object, method, args, call_site);
        }
        match object {
            Value::List(list) => self.call_method_list(list, method, args),
            Value::Dict(map) => self.call_method_dict(map, method, args),
            Value::Builtin(kind) => self.call_method_builtin(kind, method, args),
            Value::Conversation { .. } => self.call_method_conversation(object, method, args),
            Value::String(s) => self.call_method_string(s, method, args),
            Value::Stream { reader, done } => self.call_method_stream(reader, done, method, args),
            Value::Agent { .. } => self.call_method_agent(object, method, args),
            Value::Router { routes } => self.call_method_router(routes, method, args),
            Value::McpServer { tools } => self.call_method_mcp(tools, method, args),
            Value::Document { backend, .. } => self.call_method_document(backend.as_ref(), method),
            _ => Err("Can only call methods on lists, dicts, strings, conversations, streams, agents, routers, mcp_servers, documents, or builtin objects".to_string()),
        }
    }

    fn call_method_list(
        &mut self,
        list: Vec<Value>,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match method {
            // v0.30: List.crush_json(max) -> string SmartCrusher
            "crush_json" => {
                let max = args
                    .first()
                    .and_then(|v| match v {
                        Value::Float(n) => {
                            if *n < 0.0 {
                                None
                            } else {
                                Some(*n as usize)
                            }
                        }
                        _ => None,
                    })
                    .ok_or_else(|| "List.crush_json: requires max as number".to_string())?;
                let opts = crate::compress::CompressOptions::default();
                let result = crate::compress::crush_json(&list, max, &opts);
                let json =
                    crate::compress::value_to_json_simple(&Value::List(result.items.clone()));
                Ok(Value::String(format!(
                    "{}\n<compressed:method=smart_crusher strategy={} items={} total={} savings={:.2}>",
                    json,
                    result.strategy_used,
                    result.items_kept,
                    result.items_total,
                    result.savings_ratio
                )))
            }
            "push" => {
                let item = args.first().cloned().unwrap_or(Value::Nil);
                let mut new_list = list.clone();
                new_list.push(item);
                Ok(Value::List(new_list))
            }
            "get" => {
                let index = args
                    .first()
                    .and_then(|v| match v {
                        Value::Float(n) => Some(*n as usize),
                        _ => None,
                    })
                    .unwrap_or(0);
                Ok(list.get(index).cloned().unwrap_or(Value::Nil))
            }
            "pop" => {
                let mut new_list = list.clone();
                let item = new_list.pop().unwrap_or(Value::Nil);
                Ok(item)
            }
            "len" => Ok(Value::Int(list.len() as i64)),
            "map" => {
                let mapper = args.first().cloned().ok_or("map() requires a function")?;
                let mut result = Vec::new();
                for item in list {
                    let mapped = self.call_value(&mapper, vec![item])?;
                    result.push(mapped);
                }
                Ok(Value::List(result))
            }
            "filter" => {
                let predicate = args
                    .first()
                    .cloned()
                    .ok_or("filter() requires a function")?;
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
                let reducer = args
                    .first()
                    .cloned()
                    .ok_or("reduce() requires a function")?;
                let mut acc = args.get(1).cloned().unwrap_or(Value::Nil);
                for item in list {
                    acc = self.call_value(&reducer, vec![acc, item])?;
                }
                Ok(acc)
            }
            // v0.18: take(n) - 取前 n 个元素
            "take" => {
                let n = args
                    .first()
                    .and_then(|v| match v {
                        Value::Float(n) => Some(*n as usize),
                        _ => None,
                    })
                    .ok_or("take() requires a count argument")?;
                let result: Vec<Value> = list.into_iter().take(n).collect();
                Ok(Value::List(result))
            }
            // v0.18: drop(n) - 跳过前 n 个元素
            "drop" => {
                let n = args
                    .first()
                    .and_then(|v| match v {
                        Value::Float(n) => Some(*n as usize),
                        _ => None,
                    })
                    .ok_or("drop() requires a count argument")?;
                let result: Vec<Value> = list.into_iter().skip(n).collect();
                Ok(Value::List(result))
            }
            // v0.17: window(size) - 滑动窗口
            "window" => {
                let size = args
                    .first()
                    .and_then(|v| match v {
                        Value::Float(n) => Some(*n as usize),
                        _ => None,
                    })
                    .ok_or("window() requires a size argument")?;
                if size == 0 {
                    return Err("window() size must be > 0".to_string());
                }
                let mut windows = Vec::new();
                for i in 0..list.len() {
                    if i + size <= list.len() {
                        let window: Vec<Value> = list[i..i + size].to_vec();
                        windows.push(Value::List(window));
                    }
                }
                Ok(Value::List(windows))
            }
            // v0.17: batch(size) - 翻转窗口（批次处理）
            "batch" => {
                let size = args
                    .first()
                    .and_then(|v| match v {
                        Value::Float(n) => Some(*n as usize),
                        _ => None,
                    })
                    .ok_or("batch() requires a size argument")?;
                if size == 0 {
                    return Err("batch() size must be > 0".to_string());
                }
                let mut batches = Vec::new();
                for chunk in list.chunks(size) {
                    batches.push(Value::List(chunk.to_vec()));
                }
                Ok(Value::List(batches))
            }
            // v0.17: shape() - 返回维度
            "shape" => {
                fn get_shape(val: &Value) -> Vec<usize> {
                    match val {
                        Value::List(items) => {
                            if items.is_empty() {
                                vec![0]
                            } else {
                                let mut shape = vec![items.len()];
                                if let Some(first) = items.first()
                                    && let Value::List(_) = first
                                {
                                    let inner = get_shape(first);
                                    shape.extend(inner);
                                }
                                shape
                            }
                        }
                        _ => vec![],
                    }
                }
                let shape = get_shape(&Value::List(list.clone()));
                Ok(Value::List(
                    shape.iter().map(|n| Value::Float(*n as f64)).collect(),
                ))
            }
            // v0.17: flatten() - 展平嵌套列表
            "flatten" => {
                fn flatten_list(val: &Value, out: &mut Vec<Value>) {
                    match val {
                        Value::List(items) => {
                            for item in items {
                                flatten_list(item, out);
                            }
                        }
                        other => out.push(other.clone()),
                    }
                }
                let mut result = Vec::new();
                flatten_list(&Value::List(list.clone()), &mut result);
                Ok(Value::List(result))
            }
            // v0.17: transpose() - 转置二维列表
            "transpose" => {
                if list.is_empty() {
                    return Ok(Value::List(vec![]));
                }
                // 检查是否是二维列表
                let rows: Vec<&Vec<Value>> = list
                    .iter()
                    .filter_map(|v| {
                        if let Value::List(items) = v {
                            Some(items)
                        } else {
                            None
                        }
                    })
                    .collect();
                if rows.len() != list.len() {
                    return Err("transpose() requires a 2D list".to_string());
                }
                let ncols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
                let mut result = Vec::new();
                for col in 0..ncols {
                    let mut new_row = Vec::new();
                    for row in &rows {
                        new_row.push(row.get(col).cloned().unwrap_or(Value::Nil));
                    }
                    result.push(Value::List(new_row));
                }
                Ok(Value::List(result))
            }
            // v0.17: reshape(rows, cols) - 重塑列表
            "reshape" => {
                let rows = args
                    .first()
                    .and_then(|v| match v {
                        Value::Float(n) => Some(*n as usize),
                        _ => None,
                    })
                    .ok_or("reshape() requires rows argument")?;
                let cols = args
                    .get(1)
                    .and_then(|v| match v {
                        Value::Float(n) => Some(*n as usize),
                        _ => None,
                    })
                    .ok_or("reshape() requires cols argument")?;
                let total = rows * cols;
                // 展平后重塑
                fn flatten_list(val: &Value, out: &mut Vec<Value>) {
                    match val {
                        Value::List(items) => {
                            for item in items {
                                flatten_list(item, out);
                            }
                        }
                        other => out.push(other.clone()),
                    }
                }
                let mut flat = Vec::new();
                flatten_list(&Value::List(list.clone()), &mut flat);
                // 循环填充
                while flat.len() < total {
                    let extend_len = (total - flat.len()).min(flat.len());
                    let extend: Vec<Value> = flat[..extend_len].to_vec();
                    flat.extend(extend);
                }
                let mut result = Vec::new();
                for r in 0..rows {
                    let row: Vec<Value> = flat[r * cols..(r + 1) * cols].to_vec();
                    result.push(Value::List(row));
                }
                Ok(Value::List(result))
            }
            _ => Err(format!("List has no method: {}", method)),
        }
    }
    fn call_method_dict(
        &mut self,
        map: HashMap<String, Value>,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match method {
            "get" => {
                let key = args.first().map(|v| v.to_string()).unwrap_or_default();
                Ok(map.get(&key).cloned().unwrap_or(Value::Nil))
            }
            "set" => {
                let key = args.first().map(|v| v.to_string()).unwrap_or_default();
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
            "len" => Ok(Value::Int(map.len() as i64)),
            // v0.07.1: req.json() — 从 body 字段解析 JSON，返回 Result<Dict, ParseError>
            "json" => {
                let body_val = map
                    .get("body")
                    .cloned()
                    .unwrap_or(Value::String(String::new()));
                let body_str = match body_val {
                    Value::String(s) => s,
                    _ => body_val.to_string(),
                };
                if body_str.trim().is_empty() {
                    let mut err = HashMap::new();
                    err.insert(
                        "err".to_string(),
                        Value::String("ParseError: empty body".to_string()),
                    );
                    return Ok(Value::Dict(err));
                }
                match json_to_value(&body_str) {
                    Ok(val) => {
                        let mut result = HashMap::new();
                        result.insert("ok".to_string(), val);
                        Ok(Value::Dict(result))
                    }
                    Err(e) => {
                        let mut err = HashMap::new();
                        err.insert(
                            "err".to_string(),
                            Value::String(format!("ParseError: {}", e)),
                        );
                        Ok(Value::Dict(err))
                    }
                }
            }
            _ => {
                // v0.25: Skill 命名空间调用 — 直接从 Dict 中查找
                if let Some(val) = map.get(method) {
                    match val {
                        Value::Task { .. } | Value::Closure { .. } => {
                            return self.call_value(val, args);
                        }
                        _ => {
                            // 非 callable 值直接返回（如 metadata 字段）
                            if args.is_empty() {
                                return Ok(val.clone());
                            }
                        }
                    }
                }
                Err(format!("Dict has no method: {}", method))
            }
        }
    }
    fn call_method_builtin(
        &mut self,
        kind: BuiltinKind,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match (kind, method) {
            (BuiltinKind::Web, "fetch") => {
                let url = args.first().map(|v| v.to_string()).unwrap_or_default();
                // v10: 真实 HTTP GET
                self.real_web_fetch(&url)
            }
            (BuiltinKind::Json, "parse") => {
                // v10: 真实 JSON 解析
                let text = args.first().map(|v| v.to_string()).unwrap_or_default();
                json_to_value(&text).map_err(|e| format!("json.parse: {}", e))
            }
            (BuiltinKind::Json, "stringify") => {
                // v10: JSON 序列化
                let value = args.first().cloned().unwrap_or(Value::Nil);
                Ok(Value::String(value_to_json(&value)))
            }
            (BuiltinKind::File, _) => {
                // v0.25: 文件系统 builtin (file.read_text / write_text / ...)
                self.call_file_method(method, &args)
            }
            (BuiltinKind::Memory, _) => self.call_memory_method(method, &args),
            // v0.34: event bus.* (Puter EventClient 风格 wildcard)
            (BuiltinKind::Bus, _) => self.call_event_method(method, &args),
            // v0.34: sandbox.* (MimiClaw path validation + AIOS access manager)
            (BuiltinKind::Sandbox, _) => self.call_sandbox_method(method, &args),
            (BuiltinKind::Schedule, _) => self.call_schedule_method(method, &args),
            (BuiltinKind::Ccr, _) => self.call_ccr_method(method, &args),
            (BuiltinKind::Mock, _) => self.call_mock_method(method, &args),
            // v0.34: ai.tokens — expose TokenUsage counters (mini-swe-agent cost tracking)
            (BuiltinKind::AiChat, "tokens") => Ok(Value::Builtin(BuiltinKind::AiTokens)),
            (BuiltinKind::AiTokens, _) => self.call_ai_tokens_method(method, &args),
            (BuiltinKind::Agent, "create") => {
                // agent.create("name", {tools: [...], model: "deep", max_steps: 10, system: "..."})
                let name = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    _ => {
                        return Err(
                            "agent.create: first arg must be a string (agent name)".to_string()
                        );
                    }
                };
                let config = match args.get(1) {
                    Some(Value::Dict(d)) => d.clone(),
                    _ => return Err("agent.create: second arg must be a dict (config)".to_string()),
                };
                let tool_names = match config.get("tools") {
                    Some(Value::List(items)) => items.iter().map(|v| v.to_string()).collect(),
                    _ => vec![],
                };
                let model_route = match config.get("model") {
                    Some(Value::String(s)) => s.clone(),
                    _ => "default".to_string(),
                };
                let max_steps = match config.get("max_steps") {
                    Some(Value::Float(n)) => *n as usize,
                    _ => 10,
                };
                let system = match config.get("system") {
                    Some(Value::String(s)) => s.clone(),
                    _ => {
                        "You are a helpful assistant. Use the available tools to complete the task."
                            .to_string()
                    }
                };
                Ok(Value::Agent {
                    name,
                    tool_names,
                    model_route,
                    max_steps,
                    system,
                })
            }
            (BuiltinKind::Agent, "critic") => {
                // agent.critic(answer) — 评估输出质量
                // agent.critic(answer, context) — 检查是否基于上下文（幻觉检测）
                let answer = match args.first() {
                    Some(v) => v.to_string(),
                    _ => {
                        return Err(
                            "agent.critic: first arg must be the text to evaluate".to_string()
                        );
                    }
                };
                let context = args.get(1).map(|v| v.to_string());
                self.run_critic(&answer, context.as_deref())
            }
            // v0.27: 顶层模块入口 — `document.parse(path)` 返回 Value::Document
            (BuiltinKind::Document, "parse") => {
                let path = args
                    .first()
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| "document.parse: requires a path string".to_string())?;
                crate::document::parse_document(&path)
            }
            (BuiltinKind::Document, _) => Err(format!("document.{}: unknown method", method)),
            // v0.43.0: exec.* — parallel subprocess execution (pi-mono v1 inspired)
            (BuiltinKind::Exec, _) => self.call_exec_method(method, &args),
            // v0.45.0: tool.plane.* — ToolPlane Core/Extension adapter
            (BuiltinKind::Toolplane, _) => self.call_toolplane_method(method, &args),
            // v0.46.0: skill.* — MoraSkillSpec + dual registry (CLI-Anything)
            (BuiltinKind::Skill, _) => self.call_skill_method(method, &args),
            // v0.48.0: plan.* — real-time checklist (pi-agent)
            (BuiltinKind::Plan, _) => self.call_plan_method(method, &args),
            // v0.48.0: mora.* — meta (refine)
            (BuiltinKind::Mora, _) => self.call_mora_method(method, &args),
            // v0.45.0: ai.retry / ai.role — top-level AI utilities
            // (chat still handled by existing AiChat dispatch below)
            (BuiltinKind::Ai, _) => self.call_ai_method(method, &args),
            _ => Err(format!("Unknown method: {:?}.{}", kind, method)),
        }
    }
    fn call_method_conversation(
        &mut self,
        object: Value,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        let Value::Conversation {
            mut messages,
            model,
            base_url,
            api_key,
        } = object
        else {
            return Err(
                "internal: call_method_conversation called on non-Conversation".to_string(),
            );
        };
        match method {
            "chat" => {
                let prompt = args.first().map(|v| v.to_string()).unwrap_or_default();
                if prompt.is_empty() {
                    return Err("conv.chat: prompt cannot be empty".to_string());
                }
                messages.push(("user".to_string(), prompt));
                let api_key = api_key.clone();
                let model = model.clone();
                let base_url = base_url.clone();
                let response = self.real_ai_chat(&messages, &api_key, &model, &base_url)?;
                messages.push(("assistant".to_string(), response.to_string()));
                Ok(response)
            }
            "history" => {
                let hist: Vec<Value> = messages
                    .iter()
                    .map(|(role, content)| {
                        let mut m = HashMap::new();
                        m.insert("role".to_string(), Value::String(role.clone()));
                        m.insert("content".to_string(), Value::String(content.clone()));
                        Value::Dict(m)
                    })
                    .collect();
                Ok(Value::List(hist))
            }
            "clear" => {
                messages.clear();
                Ok(Value::Nil)
            }
            "model" => Ok(Value::String(model.clone())),
            "len" => Ok(Value::Int(messages.len() as i64)),
            // v0.29: Conversation.compact() 已重命名为 compress(strategy?) — 见下方 "compress" arm
            // v0.29: Conversation.compress(strategy?) -> string
            "compress" => {
                let strategy = args
                    .first()
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "summary".to_string());
                let opts = crate::compress::CompressOptions {
                    strategy: strategy.clone(),
                    ..Default::default()
                };
                crate::compress::compress_top(
                    &Value::Conversation {
                        messages: messages.clone(),
                        model: model.clone(),
                        base_url: base_url.clone(),
                        api_key: api_key.clone(),
                    },
                    &strategy,
                    &opts,
                )
            }
            _ => Err(format!("Conversation has no method: {}", method)),
        }
    }
    fn call_method_string(
        &self,
        s: String,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match method {
            "len" => Ok(Value::Float(s.len() as f64)),
            "upper" => Ok(Value::String(s.to_uppercase())),
            "lower" => Ok(Value::String(s.to_lowercase())),
            "trim" => Ok(Value::String(s.trim().to_string())),
            "starts_with" => {
                let prefix = args.first().map(|v| v.to_string()).unwrap_or_default();
                Ok(Value::Bool(s.starts_with(&prefix)))
            }
            "ends_with" => {
                let suffix = args.first().map(|v| v.to_string()).unwrap_or_default();
                Ok(Value::Bool(s.ends_with(&suffix)))
            }
            "contains" => {
                let needle = args.first().map(|v| v.to_string()).unwrap_or_default();
                Ok(Value::Bool(s.contains(&needle)))
            }
            "split" => {
                let sep = args.first().map(|v| v.to_string()).unwrap_or_default();
                let parts: Vec<Value> = s
                    .split(&sep)
                    .map(|p| Value::String(p.to_string()))
                    .collect();
                Ok(Value::List(parts))
            }
            "replace" => {
                let from = args.first().map(|v| v.to_string()).unwrap_or_default();
                let to = args.get(1).map(|v| v.to_string()).unwrap_or_default();
                Ok(Value::String(s.replace(&from, &to)))
            }
            // v0.07.3: String.json() — 与 Dict.json() 同构 API
            "json" => {
                if s.trim().is_empty() {
                    let mut err = HashMap::new();
                    err.insert(
                        "err".to_string(),
                        Value::String("ParseError: empty body".to_string()),
                    );
                    return Ok(Value::Dict(err));
                }
                match json_to_value(&s) {
                    Ok(val) => {
                        let mut result = HashMap::new();
                        result.insert("ok".to_string(), val);
                        Ok(Value::Dict(result))
                    }
                    Err(e) => {
                        let mut err = HashMap::new();
                        err.insert(
                            "err".to_string(),
                            Value::String(format!("ParseError: {}", e)),
                        );
                        Ok(Value::Dict(err))
                    }
                }
            }
            _ => Err(format!("String has no method: {}", method)),
        }
    }
    fn call_method_stream(
        &self,
        reader: StreamReader,
        done: Arc<Mutex<bool>>,
        method: &str,
        _args: Vec<Value>,
    ) -> Result<Value, String> {
        match method {
            "collect" => {
                let mut result = String::new();
                if !*done.lock() {
                    let mut guard = reader.lock();
                    loop {
                        match Self::read_next_sse_token(&mut guard) {
                            Ok(Some(token)) => result.push_str(&token),
                            Ok(None) => {
                                *done.lock() = true;
                                break;
                            }
                            Err(e) => {
                                *done.lock() = true;
                                return Err(format!("ai.stream.collect: {}", e));
                            }
                        }
                    }
                }
                Ok(Value::String(result))
            }
            "is_done" => Ok(Value::Bool(*done.lock())),
            _ => Err(format!("Stream has no method: {}", method)),
        }
    }
    fn call_method_agent(
        &mut self,
        object: Value,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        let Value::Agent {
            name,
            tool_names,
            model_route,
            max_steps,
            system,
        } = object
        else {
            return Err("internal: call_method_agent called on non-Agent".to_string());
        };
        match method {
            "run" => {
                let task = args.first().map(|v| v.to_string()).unwrap_or_default();
                if task.is_empty() {
                    return Err("agent.run: first arg must be a string (task)".to_string());
                }
                // 克隆需要的数据（避免借用冲突）
                let agent_name = name.clone();
                let agent_tools = tool_names.clone();
                let agent_route = model_route.clone();
                let agent_max = max_steps;
                let agent_system = system.clone();
                self.run_agent(
                    &agent_name,
                    &agent_tools,
                    &agent_route,
                    agent_max,
                    &agent_system,
                    &task,
                )
            }
            "name" => Ok(Value::String(name.clone())),
            "max_steps" => Ok(Value::Float(max_steps as f64)),
            _ => Err(format!("Agent has no method: {}", method)),
        }
    }
    fn call_method_router(
        &mut self,
        routes: Arc<Mutex<Vec<(String, String, Value)>>>,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        let mut r = routes.lock();
        match method {
            "route" => {
                let http_method = args
                    .first()
                    .map(|v| v.to_string())
                    .unwrap_or_default()
                    .to_uppercase();
                let path = args.get(1).map(|v| v.to_string()).unwrap_or_default();
                let handler = args
                    .get(2)
                    .cloned()
                    .ok_or("Router.route() requires a handler")?;
                r.push((http_method, path, handler));
                Ok(Value::Router {
                    routes: routes.clone(),
                })
            }
            "listen" => {
                // S6 fix: 默认绑定 127.0.0.1（仅本机），避免开发服务暴露公网。
                // 用户需公网暴露时显式传 "0.0.0.0:3000"。
                let addr = args
                    .first()
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "127.0.0.1:3000".to_string());
                let (host, port) = addr.split_once(':').unwrap_or(("127.0.0.1", "3000"));
                let port: u16 = port
                    .parse()
                    .map_err(|_| format!("Invalid port: {}", port))?;
                let r_clone: Vec<(String, String, Value)> = r.clone();
                drop(r);
                eprintln!("[Router] starting HTTP server on {}", addr);
                let interp_arc: Arc<tokio::sync::RwLock<Interpreter>> =
                    Arc::new(tokio::sync::RwLock::new(self.clone()));
                block_on_async(async {
                    crate::http_server::start(
                        host,
                        port,
                        Arc::new(tokio::sync::RwLock::new(
                            r_clone
                                .iter()
                                .map(|(m, p, h)| ((m.clone(), p.clone()), h.clone()))
                                .collect(),
                        )),
                        interp_arc,
                    )
                    .await
                })
                .map_err(|e| format!("HTTP server error: {}", e))?;
                Ok(Value::Nil)
            }
            _ => {
                drop(r);
                Err(format!("Router has no method: {}", method))
            }
        }
    }
    fn call_method_mcp(
        &mut self,
        mut tools: Vec<(String, Value)>,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match method {
            "tool" => {
                let name = args.first().map(|v| v.to_string()).unwrap_or_default();
                let handler = args
                    .get(2)
                    .cloned()
                    .ok_or("McpServer.tool() requires 3 args (name, schema, handler)")?;
                tools.push((name, handler));
                Ok(Value::McpServer {
                    tools: tools.clone(),
                })
            }
            "serve" => {
                let tools_clone = tools.clone();
                eprintln!(
                    "[McpServer] starting MCP server on stdio ({} tools)",
                    tools_clone.len()
                );
                block_on_async(async {
                    let tool_registry: Arc<
                        tokio::sync::RwLock<HashMap<String, crate::mcp_server::McpTool>>,
                    > = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
                    {
                        let mut tr = tool_registry.write().await;
                        for (name, handler) in tools_clone {
                            let mcp_tool = crate::mcp_server::McpTool {
                                name: name.clone(),
                                description: String::new(),
                                parameters: "{}".to_string(),
                                handler,
                                toolset: "custom".to_string(),
                            };
                            tr.insert(name, mcp_tool);
                        }
                    }
                    let interp_arc: Arc<tokio::sync::RwLock<Interpreter>> =
                        Arc::new(tokio::sync::RwLock::new(self.clone()));
                    crate::mcp_server::start(tool_registry, interp_arc, None).await
                })
                .map_err(|e| format!("MCP server error: {}", e))?;
                Ok(Value::Nil)
            }
            _ => Err(format!("McpServer has no method: {}", method)),
        }
    }
    fn call_method_document(
        &self,
        backend: &dyn crate::document::DocumentBackend,
        method: &str,
    ) -> Result<Value, String> {
        match method {
            "markdown" => backend.markdown().map(Value::String),
            "text" => backend.text().map(Value::String),
            "pages" => backend.pages(),
            "metadata" => backend.metadata(),
            "blocks" => backend.blocks(),
            "origin" => Ok(Value::String(backend.origin().to_string())),
            other => Err(format!(
                "document.{}: unknown method on Document value",
                other
            )),
        }
    }

    pub(crate) fn call_value(&mut self, value: &Value, args: Vec<Value>) -> Result<Value, String> {
        match value {
            // α.10: MIR-built closure — 走 run_mir。
            Value::Closure {
                mir_body,
                params,
                env,
                ..
            } => {
                if args.len() < params.len() {
                    return Err(format!(
                        "closure expects {} args, got {}",
                        params.len(),
                        args.len()
                    ));
                }
                let mut child_env = Environment::with_parent_of(std::sync::Arc::new(
                    parking_lot::Mutex::new(env.0.as_ref().clone()),
                ));
                for (i, param) in params.iter().enumerate() {
                    let val = args.get(i).cloned().unwrap_or(Value::Nil);
                    child_env.define(param.clone(), val, false);
                }
                crate::mir::vm::run_mir(mir_body, self, &mut child_env)
            }
            // α.11: MIR-built task — 走 run_mir。
            Value::Task {
                mir_body, params, ..
            } => {
                if args.len() < params.len() {
                    return Err(format!(
                        "task expects {} args, got {}",
                        params.len(),
                        args.len()
                    ));
                }
                let mut child_env = Environment::with_parent_of(self.core.environment.clone());
                for (i, param) in params.iter().enumerate() {
                    let val = args.get(i).cloned().unwrap_or(Value::Nil);
                    child_env.define(param.clone(), val, false);
                }
                crate::mir::vm::run_mir(mir_body, self, &mut child_env)
            }
            // α.10: Compose/Partial 链路递归 call_value。
            Value::Compose(funcs) => {
                let mut result = args;
                for f in funcs {
                    result = vec![self.call_value(f, result)?];
                }
                Ok(result.into_iter().next().unwrap_or(Value::Nil))
            }
            Value::Partial(func, partial_args) => {
                let mut all_args = partial_args.clone();
                all_args.extend(args);
                self.call_value(func, all_args)
            }
            _ => Err(format!("Value is not callable: {}", value)),
        }
    }
}

// ===================================================================
// v0.26: compose_prompt / tail 辅助函数 (在 dispatch.rs 末尾)
// ===================================================================

/// 把 Value 转 String (用于 section.text 字段读取)
fn text_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Float(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Nil => String::new(),
        other => other.to_string(),
    }
}

/// 解析 budget 值 (dispatch 层副本,与 execute.rs 同语义)
fn parse_budget_dispatch(v: Value, ctx: &str) -> Result<usize, String> {
    match v {
        Value::Float(n) => {
            if n < 0.0 {
                return Err(format!("{}: budget must be non-negative", ctx));
            }
            Ok(n as usize)
        }
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return Err(format!("{}: empty budget string", ctx));
            }
            let bytes = s.as_bytes();
            let mut i = 0;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let num_part = &s[..i];
            let unit_part = s[i..].trim();
            let num: f64 = num_part
                .parse()
                .map_err(|_| format!("{}: invalid budget '{}'", ctx, s))?;
            let mult: usize = match unit_part.to_uppercase().as_str() {
                "" | "B" => 1,
                "KB" | "K" => 1024,
                "MB" | "M" => 1024 * 1024,
                "GB" | "G" => 1024 * 1024 * 1024,
                other => {
                    return Err(format!(
                        "{}: unknown budget unit '{}' (B/KB/MB/GB)",
                        ctx, other
                    ));
                }
            };
            Ok((num * mult as f64) as usize)
        }
        other => Err(format!(
            "{}: budget must be string or number, got {:?}",
            ctx, other
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interpreter::Interpreter;

    #[test]
    fn merge_with_builtin_sets_per_key_strategy() {
        // v0.75.23: merge_with(key, strategy) 写侧 — 解析策略名并插入
        // current_merge_strategies（读侧 run_isolated 已接；此前无生产者）。
        let mut interp = Interpreter::new();
        let env = interp.take_env();
        interp
            .call_function(
                "merge_with",
                vec![
                    Value::String("x".to_string()),
                    Value::String("grow_only_set".to_string()),
                ],
                &env,
                Span::default(),
            )
            .expect("merge_with should succeed");
        let strategies = interp.current_merge_strategies().expect("strategies set");
        assert_eq!(
            strategies.get("x"),
            Some(&crate::value::MergeStrategy::GrowOnlySet)
        );
    }

    #[test]
    fn merge_with_accumulates_multiple_keys() {
        let mut interp = Interpreter::new();
        let env = interp.take_env();
        for (k, s) in [
            ("a", "append"),
            ("b", "add"),
            ("c", "dict_union"),
            ("d", "lww"),
        ] {
            interp
                .call_function(
                    "merge_with",
                    vec![Value::String(k.to_string()), Value::String(s.to_string())],
                    &env,
                    Span::default(),
                )
                .expect("merge_with should succeed");
        }
        let strategies = interp.current_merge_strategies().expect("strategies set");
        assert_eq!(strategies.len(), 4, "多次调用应累积 per-key 策略");
        assert_eq!(
            strategies.get("a"),
            Some(&crate::value::MergeStrategy::Append)
        );
        assert_eq!(strategies.get("b"), Some(&crate::value::MergeStrategy::Add));
    }

    #[test]
    fn merge_with_unknown_strategy_errors() {
        let mut interp = Interpreter::new();
        let env = interp.take_env();
        let err = interp
            .call_function(
                "merge_with",
                vec![
                    Value::String("x".to_string()),
                    Value::String("bogus".to_string()),
                ],
                &env,
                Span::default(),
            )
            .unwrap_err();
        assert!(err.contains("unknown strategy"), "got: {}", err);
    }

    /// v0.75.49: testcase! 标注的分支覆盖 —— 每个插桩守卫都有真实可达
    /// 用例（SQLite testcase() 精神：分支可审计）。debug 构建下守卫若被
    /// 意外绕过会 panic（debug_assert），此测试确保插桩分支在正常调用下
    /// 全部命中。
    #[test]
    fn testcase_instrumented_branches_reachable() {
        let mut interp = Interpreter::new();
        let env = interp.take_env();
        // len: list / string / dict 三分支
        let n = interp
            .call_function("len", vec![Value::List(vec![])], &env, Span::default())
            .unwrap();
        assert_eq!(n, Value::Int(0));
        let n = interp
            .call_function(
                "len",
                vec![Value::String("ab".into())],
                &env,
                Span::default(),
            )
            .unwrap();
        assert_eq!(n, Value::Int(2));
        let n = interp
            .call_function(
                "len",
                vec![Value::Dict(Default::default())],
                &env,
                Span::default(),
            )
            .unwrap();
        assert_eq!(n, Value::Int(0));
        // merge_with: string key + string strategy 两守卫
        interp
            .call_function(
                "merge_with",
                vec![Value::String("k".into()), Value::String("append".into())],
                &env,
                Span::default(),
            )
            .expect("merge_with should succeed");
    }

    /// v0.75.52: BuiltinKind::from_name 静态表覆盖（P6）—— 26 kind 全可查，
    /// 未登记名返回 None（fallback）。
    #[test]
    fn builtin_kind_from_name_coverage() {
        use crate::value::BuiltinKind;
        for (name, kind) in [
            ("print", BuiltinKind::Print),
            ("range", BuiltinKind::Range),
            ("len", BuiltinKind::Len),
            ("file.read_text", BuiltinKind::File),
            ("memory.store", BuiltinKind::Memory),
            ("ai.chat", BuiltinKind::AiChat),
            ("ai.tokens", BuiltinKind::AiTokens),
            ("ai.retry", BuiltinKind::Ai),
            ("web.fetch", BuiltinKind::Web),
            ("json.parse", BuiltinKind::Json),
            ("ccr.put", BuiltinKind::Ccr),
            ("plan.update", BuiltinKind::Plan),
            ("mora.refine", BuiltinKind::Mora),
        ] {
            assert_eq!(
                BuiltinKind::from_name(name),
                Some(kind),
                "from_name({name})"
            );
        }
        assert_eq!(BuiltinKind::from_name("no_such_fn"), None, "未登记应 None");
    }
}
