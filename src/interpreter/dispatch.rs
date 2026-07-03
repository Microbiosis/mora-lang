//! 函数分发模块

use super::*;
use crate::common::Span;
use crate::value::Value;

impl Interpreter {
    pub(super) fn call_function(
        &mut self,
        name: &str,
        args: Vec<Value>,
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
            if self.trait_registry.contains_key(&trait_name) {
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
        match name {
            "print" => {
                let msg = args
                    .into_iter()
                    .map(|v| v.to_string())
                    .collect::<Vec<_>>()
                    .join("\t");
                println!("{}", msg);
                Ok(Value::Nil)
            }
            "range" => {
                let start = args
                    .first()
                    .and_then(|v| match v {
                        Value::Number(n) => Some(*n as i64),
                        _ => None,
                    })
                    .unwrap_or(0);
                let end = args
                    .get(1)
                    .and_then(|v| match v {
                        Value::Number(n) => Some(*n as i64),
                        _ => None,
                    })
                    .unwrap_or(start);
                let step = args
                    .get(2)
                    .and_then(|v| match v {
                        Value::Number(n) => Some(*n as i64),
                        _ => None,
                    })
                    .unwrap_or(1);
                let mut items = Vec::new();
                let mut i = start;
                while i < end {
                    items.push(Value::Number(i as f64));
                    i += step;
                }
                Ok(Value::List(items))
            }
            "len" => {
                let len = match args.first() {
                    Some(Value::List(list)) => list.len(),
                    Some(Value::String(s)) => s.len(),
                    Some(Value::Dict(map)) => map.len(),
                    _ => return Err("len() expects a list, string, or dict".to_string()),
                };
                Ok(Value::Number(len as f64))
            }
            // v0.17: compose(f1, f2, f3) → fn(x) = f3(f2(f1(x)))
            "compose" => {
                if args.is_empty() {
                    return Err("compose() requires at least 1 argument".to_string());
                }
                // 返回一个特殊的 Compose 值
                Ok(Value::Compose(args))
            }
            // v0.18: partial(fn, args...) → 部分应用
            "partial" => {
                if args.is_empty() {
                    return Err("partial() requires at least 1 argument (the function)".to_string());
                }
                let func = args[0].clone();
                let partial_args: Vec<Value> = args[1..].to_vec();
                Ok(Value::Partial(Box::new(func), partial_args))
            }
            // v0.19: atom(value) → 创建可变引用
            "atom" => {
                let value = args.first().cloned().unwrap_or(Value::Nil);
                Ok(Value::Atom(Arc::new(Mutex::new(value))))
            }
            // v0.19: swap(atom, fn) → 原子更新
            "swap" => {
                if args.len() < 2 {
                    return Err("swap() requires 2 arguments: atom and function".to_string());
                }
                match &args[0] {
                    Value::Atom(arc) => {
                        let func = &args[1];
                        let old = arc.lock().expect("atom mutex poisoned").clone();
                        let new_val = self.call_value(func, vec![old])?;
                        *arc.lock().expect("atom mutex poisoned") = new_val.clone();
                        Ok(new_val)
                    }
                    _ => Err("swap() first argument must be an atom".to_string()),
                }
            }
            // v0.19: deref(atom) → 读取引用值
            "deref" => {
                let value = args.first().ok_or("deref() requires 1 argument")?;
                match value {
                    Value::Atom(arc) => Ok(arc.lock().expect("atom mutex poisoned").clone()),
                    _ => Err("deref() argument must be an atom".to_string()),
                }
            }
            // v0.20: type_of(value) → 返回类型名
            "type_of" => {
                let value = args.first().ok_or("type_of() requires 1 argument")?;
                Ok(Value::String(value_type_name(value).to_string()))
            }
            // v0.20: is_instance(value, type_name) → 类型检查
            "is_instance" => {
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
            // v0.20: methods_of(value) → 返回方法名列表
            "methods_of" => {
                let value = args.first().ok_or("methods_of() requires 1 argument")?;
                let methods = get_methods_for_value(value);
                Ok(Value::List(
                    methods.into_iter().map(Value::String).collect(),
                ))
            }
            // v0.29: compress(input, strategy, options?) -> string 6 路策略压缩
            "compress" => {
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
            // v0.29: crush_json(input, max, options?) -> string Kneedle + 异常保留
            "crush_json" => {
                if args.len() < 2 {
                    return Err("crush_json() requires 2 arguments: input and max".to_string());
                }
                let max_items = match &args[1] {
                    Value::Number(n) => {
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
            // v0.24: batch_chat(prompts) -> list<string> 批量 AI 调用
            "batch_chat" => {
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
                            let result = Self::do_ai_chat(self, "gpt-4o-mini", &prompt)?;
                            results.push(result);
                        }
                        Ok(Value::List(results))
                    }
                    _ => Err("batch_chat() argument must be a list".to_string()),
                }
            }
            // v0.17: into(collection, fn) → 应用 fn 到集合的每个元素
            "into" => {
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
            // v0.06.3: Router::new() builtin
            "Router::new" => Ok(Value::Router {
                routes: Arc::new(Mutex::new(Vec::new())),
            }),
            // v0.06.6: McpServer::new() builtin
            "McpServer::new" => Ok(Value::McpServer { tools: Vec::new() }),
            // v0.26: tail(path, max: N) builtin — 读文件末 N 行(JSONL/纯文本皆可)
            "tail" => {
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
                    Value::Number(n) => {
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
            // v0.26: compose_prompt(...) builtin — 把多个 section 拼成 system prompt
            // 入参形态: (a) 已声明的 section name(String)
            //          (b) 字典 {role, text, budget}
            //          (c) 直接的 Value::PromptSection
            "compose_prompt" => {
                if args.is_empty() {
                    return Err("compose_prompt() requires at least 1 section".to_string());
                }
                let mut buf = String::new();
                for arg in args {
                    let (name, role, text, budget_bytes) = match arg {
                        Value::String(section_name) => {
                            // 从环境查 section
                            let looked_up = self
                                .environment
                                .lock()
                                .expect("environment mutex poisoned")
                                .get(&section_name);
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
            _ => {
                // 先 clone 出值，释放 borrow，避免借用冲突
                let looked_up = self
                    .environment
                    .lock()
                    .expect("environment mutex poisoned")
                    .get(name)
                    .clone();
                if let Some(value) = looked_up {
                    match value {
                        Value::Task { .. }
                        | Value::Closure { .. }
                        | Value::Compose(_)
                        | Value::Partial(_, _) => self.call_value(&value, args),
                        Value::Macro { params, .. } => {
                            let env = Arc::new(Mutex::new(Environment::with_parent(
                                self.environment.clone(),
                            )));
                            for (i, param) in params.iter().enumerate() {
                                let value = args.get(i).cloned().unwrap_or(Value::Nil);
                                env.lock().expect("env").define(param.clone(), value, false);
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
        }
    }

    /// v0.17: 直接调用 Value 形式的函数（用于管道闭包）
    #[allow(dead_code)]
    pub(super) fn call_method(
        &mut self,
        mut object: Value,
        method: &str,
        args: Vec<Value>,
        call_site: Span,
    ) -> Result<Value, String> {
        // v0.22: 方法调用内联缓存
        let _cache_key = format!("{}:{}", type_name(&object), method);
        // 注：内联缓存主要优化方法查找，实际执行仍需分派

        // v0.08.5: dyn dispatch —— TraitObject 走 dispatch_trait_method（按 for_type + trait_name 选 impl）
        // call_site 透传给 dispatcher，dispatch 失败时报错带行号方便定位
        if let Value::TraitObject { .. } = &object {
            return self.dispatch_trait_method(&object, method, args, call_site);
        }
        match object {
            Value::List(list) => {
                match method {
                    // v0.30: List.crush_json(max) -> string SmartCrusher
                    "crush_json" => {
                        let max = args
                            .first()
                            .and_then(|v| match v {
                                Value::Number(n) => {
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
                        let json = crate::compress::value_to_json_simple(&Value::List(
                            result.items.clone(),
                        ));
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
                        let index = args.first().and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None }).unwrap_or(0);
                        Ok(list.get(index).cloned().unwrap_or(Value::Nil))
                    }
                    "pop" => {
                        let mut new_list = list.clone();
                        let item = new_list.pop().unwrap_or(Value::Nil);
                        Ok(item)
                    }
                    "len" => Ok(Value::Number(list.len() as f64)),
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
                        let predicate = args.first().cloned().ok_or("filter() requires a function")?;
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
                        let reducer = args.first().cloned().ok_or("reduce() requires a function")?;
                        let mut acc = args.get(1).cloned().unwrap_or(Value::Nil);
                        for item in list {
                            acc = self.call_value(&reducer, vec![acc, item])?;
                        }
                        Ok(acc)
                    }
                    // v0.18: take(n) - 取前 n 个元素
                    "take" => {
                        let n = args.first()
                            .and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None })
                            .ok_or("take() requires a count argument")?;
                        let result: Vec<Value> = list.into_iter().take(n).collect();
                        Ok(Value::List(result))
                    }
                    // v0.18: drop(n) - 跳过前 n 个元素
                    "drop" => {
                        let n = args.first()
                            .and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None })
                            .ok_or("drop() requires a count argument")?;
                        let result: Vec<Value> = list.into_iter().skip(n).collect();
                        Ok(Value::List(result))
                    }
                    // v0.17: window(size) - 滑动窗口
                    "window" => {
                        let size = args.first()
                            .and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None })
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
                        let size = args.first()
                            .and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None })
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
                                            && let Value::List(_) = first {
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
                        Ok(Value::List(shape.iter().map(|n| Value::Number(*n as f64)).collect()))
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
                        let rows: Vec<&Vec<Value>> = list.iter().filter_map(|v| {
                            if let Value::List(items) = v { Some(items) } else { None }
                        }).collect();
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
                        let rows = args.first()
                            .and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None })
                            .ok_or("reshape() requires rows argument")?;
                        let cols = args.get(1)
                            .and_then(|v| match v { Value::Number(n) => Some(*n as usize), _ => None })
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
            Value::Dict(map) => {
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
                    "len" => Ok(Value::Number(map.len() as f64)),
                    // v0.07.1: req.json() — 从 body 字段解析 JSON，返回 Result<Dict, ParseError>
                    "json" => {
                        let body_val = map.get("body").cloned().unwrap_or(Value::String(String::new()));
                        let body_str = match body_val {
                            Value::String(s) => s,
                            _ => body_val.to_string(),
                        };
                        if body_str.trim().is_empty() {
                            let mut err = HashMap::new();
                            err.insert("err".to_string(), Value::String("ParseError: empty body".to_string()));
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
                                err.insert("err".to_string(), Value::String(format!("ParseError: {}", e)));
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
            Value::Builtin(name) => match (name.as_str(), method) {
                ("web", "fetch") => {
                    let url = args.first().map(|v| v.to_string()).unwrap_or_default();
                    // v10: 真实 HTTP GET
                    self.real_web_fetch(&url)
                }
                ("json", "parse") => {
                    // v10: 真实 JSON 解析
                    let text = args.first().map(|v| v.to_string()).unwrap_or_default();
                    json_to_value(&text).map_err(|e| format!("json.parse: {}", e))
                }
                ("json", "stringify") => {
                    // v10: JSON 序列化
                    let value = args.first().cloned().unwrap_or(Value::Nil);
                    Ok(Value::String(value_to_json(&value)))
                }
                ("file", method) => self.call_file_method(method, &args),
                ("memory", method) => self.call_memory_method(method, &args),
                // v0.34: event bus.* (Puter EventClient 风格 wildcard)
                ("bus", method) => self.call_event_method(method, &args),
                // v0.34: sandbox.* (MimiClaw path validation + AIOS access manager)
                ("sandbox", method) => self.call_sandbox_method(method, &args),
                ("schedule", method) => self.call_schedule_method(method, &args),
                ("ccr", method) => self.call_ccr_method(method, &args),
                ("agent", "create") => {
                    // agent.create("name", {tools: [...], model: "deep", max_steps: 10, system: "..."})
                    let name = match args.first() {
                        Some(Value::String(s)) => s.clone(),
                        _ => return Err("agent.create: first arg must be a string (agent name)".to_string()),
                    };
                    let config = match args.get(1) {
                        Some(Value::Dict(d)) => d.clone(),
                        _ => return Err("agent.create: second arg must be a dict (config)".to_string()),
                    };
                    let tool_names = match config.get("tools") {
                        Some(Value::List(items)) => {
                            items.iter().map(|v| v.to_string()).collect()
                        }
                        _ => vec![],
                    };
                    let model_route = match config.get("model") {
                        Some(Value::String(s)) => s.clone(),
                        _ => "default".to_string(),
                    };
                    let max_steps = match config.get("max_steps") {
                        Some(Value::Number(n)) => *n as usize,
                        _ => 10,
                    };
                    let system = match config.get("system") {
                        Some(Value::String(s)) => s.clone(),
                        _ => "You are a helpful assistant. Use the available tools to complete the task.".to_string(),
                    };
                    Ok(Value::Agent { name, tool_names, model_route, max_steps, system })
                }
                ("agent", "critic") => {
                    // agent.critic(answer) — 评估输出质量
                    // agent.critic(answer, context) — 检查是否基于上下文（幻觉检测）
                    let answer = match args.first() {
                        Some(v) => v.to_string(),
                        _ => return Err("agent.critic: first arg must be the text to evaluate".to_string()),
                    };
                    let context = args.get(1).map(|v| v.to_string());
                    self.run_critic(&answer, context.as_deref())
                }
                // v0.27: 顶层模块入口 — `document.parse(path)` 返回 Value::Document
                ("document", "parse") => {
                    let path = args
                        .first()
                        .and_then(|v| match v {
                            Value::String(s) => Some(s.clone()),
                            _ => None,
                        })
                        .ok_or_else(|| "document.parse: requires a path string".to_string())?;
                    crate::document::parse_document(&path)
                }
                ("document", method) => Err(format!("document.{}: unknown method", method)),
                _ => Err(format!("Unknown method: {}.{}", name, method)),
            },
            Value::Conversation { ref mut messages, ref model, ref base_url, ref api_key } => {
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
                        crate::compress::compress_top(&object, &strategy, &opts)
                    }
                    _ => Err(format!("Conversation has no method: {}", method)),
                }
            }
            // v0.07.1: String.json() — 解析 JSON 字符串，返回 Result<Value, ParseError>
            Value::String(s) => {
                match method {
                    "len" => Ok(Value::Number(s.len() as f64)),
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
                        let parts: Vec<Value> = s.split(&sep)
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
                            err.insert("err".to_string(), Value::String("ParseError: empty body".to_string()));
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
                                err.insert("err".to_string(), Value::String(format!("ParseError: {}", e)));
                                Ok(Value::Dict(err))
                            }
                        }
                    }
                    _ => Err(format!("String has no method: {}", method)),
                }
            }
            Value::Stream { ref reader, ref done } => {
                match method {
                    "collect" => {
                        let mut result = String::new();
                        if !*done.lock().expect("done mutex poisoned") {
                            let mut guard = reader.lock();
                            loop {
                                match Self::read_next_sse_token(&mut guard) {
                                    Ok(Some(token)) => result.push_str(&token),
                                    Ok(None) => {
                                        *done.lock().expect("done mutex poisoned") = true;
                                        break;
                                    }
                                    Err(e) => {
                                        *done.lock().expect("done mutex poisoned") = true;
                                        return Err(format!("ai.stream.collect: {}", e));
                                    }
                                }
                            }
                        }
                        Ok(Value::String(result))
                    }
                    "is_done" => {
                        Ok(Value::Bool(*done.lock().expect("done mutex poisoned")))
                    }
                    _ => Err(format!("Stream has no method: {}", method)),
                }
            }
            Value::Agent { ref name, ref tool_names, ref model_route, max_steps, ref system } => {
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
                        self.run_agent(&agent_name, &agent_tools, &agent_route, agent_max, &agent_system, &task)
                    }
                    "name" => Ok(Value::String(name.clone())),
                    "max_steps" => Ok(Value::Number(max_steps as f64)),
                    _ => Err(format!("Agent has no method: {}", method)),
                }
            }
            // v0.06.3: Router 方法
            Value::Router { ref mut routes } => {
                let mut r = routes.lock().expect("routes mutex poisoned");
                match method {
                    "route" => {
                        let http_method = args.first().map(|v| v.to_string()).unwrap_or_default().to_uppercase();
                        let path = args.get(1).map(|v| v.to_string()).unwrap_or_default();
                        let handler = args.get(2).cloned().ok_or("Router.route() requires a handler")?;
                        r.push((http_method, path, handler));
                        Ok(Value::Router { routes: routes.clone() })
                    }
                    "listen" => {
                        let addr = args.first().map(|v| v.to_string()).unwrap_or_else(|| "0.0.0.0:3000".to_string());
                        let (host, port) = addr.split_once(':').unwrap_or(("0.0.0.0", "3000"));
                        let port: u16 = port.parse().map_err(|_| format!("Invalid port: {}", port))?;
                        let r_clone: Vec<(String, String, Value)> = r.clone();
                        drop(r);
                        eprintln!("[Router] starting HTTP server on {}", addr);
                        let interp_arc: Arc<Mutex<Interpreter>> = Arc::new(Mutex::new(self.clone()));
                        crate::http_server::start(
                            host, port,
                            Arc::new(Mutex::new(r_clone.iter().map(|(m,p,h)|
                                ((m.clone(), p.clone()), h.clone())
                            ).collect())),
                            interp_arc,
                        ).map_err(|e| format!("HTTP server error: {}", e))?;
                        Ok(Value::Nil)
                    }
                    _ => { drop(r); Err(format!("Router has no method: {}", method)) },
                }
            }
            // v0.06.6: McpServer 方法
            Value::McpServer { ref mut tools } => {
                match method {
                    "tool" => {
                        let name = args.first().map(|v| v.to_string()).unwrap_or_default();
                        let handler = args.get(2).cloned().ok_or("McpServer.tool() requires 3 args (name, schema, handler)")?;
                        tools.push((name, handler));
                        Ok(Value::McpServer { tools: tools.clone() })
                    }
                    "serve" => {
                        let tools_clone = tools.clone();
                        eprintln!("[McpServer] starting MCP server on stdio ({} tools)", tools_clone.len());
                        let tool_registry: Arc<Mutex<HashMap<String, crate::mcp_server::McpTool>>> =
                            Arc::new(Mutex::new(HashMap::new()));
                        for (name, handler) in tools_clone {
                            let mcp_tool = crate::mcp_server::McpTool {
                                name: name.clone(),
                                description: String::new(),
                                parameters: "{}".to_string(),
                                handler,
                                toolset: "custom".to_string(),
                            };
                            tool_registry.lock().expect("tool_registry mutex poisoned").insert(name, mcp_tool);
                        }
                        let interp_arc: Arc<Mutex<Interpreter>> = Arc::new(Mutex::new(self.clone()));
                        crate::mcp_server::start(tool_registry, interp_arc, None)
                            .map_err(|e| format!("MCP server error: {}", e))?;
                        Ok(Value::Nil)
                    }
                    _ => Err(format!("McpServer has no method: {}", method)),
                }
            }
            // v0.27: Document unified IR — value-method dispatch on DocumentBackend
            Value::Document { backend, .. } => {
                let _ = (args, call_site);
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
            _ => Err("Can only call methods on lists, dicts, strings, conversations, streams, agents, routers, mcp_servers, documents, or builtin objects".to_string()),
        }
    }

    pub(crate) fn call_value(&mut self, value: &Value, args: Vec<Value>) -> Result<Value, String> {
        match value {
            Value::Closure { v2_node_id, .. } => {
                if v2_node_id.is_some() {
                    if let Some(ref arena) = self.v2_arena.clone() {
                        return self.call_value_inner(value, args, arena);
                    }
                    return Err("v2 closure requires arena".to_string());
                }
                Err("v1 closure not supported in v2 mode".to_string())
            }
            Value::Task { v2_body_ids, .. } if v2_body_ids.is_empty() => {
                Err("v1 task not supported in v2 mode".to_string())
            }
            Value::Task {
                params,
                v2_body_ids,
                ..
            } => {
                if let Some(ref arena) = self.v2_arena.clone() {
                    return self.call_task_inner(params, v2_body_ids, args, arena);
                }
                Err("v2 task requires arena".to_string())
            }
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

    /// v2 版 call_task —— 通过 arena 执行 task body
    pub(super) fn call_task_inner(
        &mut self,
        params: &[String],
        body_ids: &[usize],
        args: Vec<Value>,
        arena: &crate::ast_v2::AstArena,
    ) -> Result<Value, String> {
        let call_env = Arc::new(Mutex::new(Environment::with_parent(
            self.environment.clone(),
        )));
        for (i, param) in params.iter().enumerate() {
            let value = args.get(i).cloned().unwrap_or(Value::Nil);
            call_env
                .lock()
                .expect("env")
                .define(param.clone(), value, false);
        }
        let prev_env = self.environment.clone();
        self.environment = call_env;
        // 单表达式 body：直接返回表达式值（与 closure 行为一致）
        if body_ids.len() == 1
            && let Some(stmt) = arena.get_stmt(crate::ast_v2::NodeId(body_ids[0]))
            && let crate::ast_v2::StmtKind::Expr(expr_id) = &stmt.kind
        {
            let result = self.evaluate(*expr_id, arena);
            self.environment = prev_env;
            return result;
        }
        for body_idx in body_ids {
            let body_id = crate::ast_v2::NodeId(*body_idx);
            if let Some(stmt) = arena.get_stmt(body_id) {
                let kind = stmt.kind.clone();
                match self.execute(&kind, arena)? {
                    FlowSignal::None => {}
                    FlowSignal::Return(val) => {
                        self.environment = prev_env;
                        return Ok(val);
                    }
                    signal => {
                        self.environment = prev_env;
                        return Err(format!("Unexpected signal in task: {:?}", signal));
                    }
                }
            }
        }
        self.environment = prev_env;
        Ok(Value::Nil)
    }

    /// v2 版 call_value —— 支持通过 arena 执行 v2 闭包
    pub(crate) fn call_value_inner(
        &mut self,
        value: &Value,
        args: Vec<Value>,
        arena: &crate::ast_v2::AstArena,
    ) -> Result<Value, String> {
        match value {
            Value::Closure {
                env, v2_node_id, ..
            } => {
                if let Some(node_id) = v2_node_id {
                    // v2 闭包: 从 arena 获取 body 并执行
                    let node_id = crate::ast_v2::NodeId(*node_id);
                    // node_id 可能是 ExprKind::Closure 或 StmtKind 中的闭包表达式
                    let closure_info = arena.get_expr(node_id).and_then(|expr| {
                        if let crate::ast_v2::ExprKind::Closure { params, body, .. } = &expr.kind {
                            Some((params.clone(), body.clone()))
                        } else {
                            None
                        }
                    });
                    if let Some((closure_params, closure_body)) = closure_info {
                        // 使用子环境（避免 mutex 死锁）
                        let call_env = std::sync::Arc::new(std::sync::Mutex::new(
                            crate::value::Environment::with_parent(env.clone()),
                        ));
                        // 绑定参数
                        for (i, (pname, _)) in closure_params.iter().enumerate() {
                            let val = args.get(i).cloned().unwrap_or(Value::Nil);
                            call_env
                                .lock()
                                .expect("env")
                                .define(pname.clone(), val, false);
                        }
                        let prev_env = self.environment.clone();
                        self.environment = call_env;
                        // 执行 body
                        let result = Value::Nil;
                        for body_id in &closure_body {
                            if let Some(stmt) = arena.get_stmt(*body_id) {
                                let kind = stmt.kind.clone();
                                match self.execute(&kind, arena)? {
                                    FlowSignal::None => {}
                                    FlowSignal::Return(val) => {
                                        self.environment = prev_env;
                                        return Ok(val);
                                    }
                                    signal => {
                                        self.environment = prev_env;
                                        return Err(format!(
                                            "Unexpected signal in closure: {:?}",
                                            signal
                                        ));
                                    }
                                }
                            }
                        }
                        self.environment = prev_env;
                        return Ok(result);
                    }
                    Err(format!("Invalid v2 closure node: {}", node_id.0))
                } else {
                    Err("v1 closure not supported in v2 mode".to_string())
                }
            }
            Value::Task { .. } => {
                Err("v1 task not supported in v2 mode, use call_value".to_string())
            }
            Value::Compose(funcs) => {
                let mut result = args;
                for f in funcs {
                    result = vec![self.call_value_inner(f, result, arena)?];
                }
                Ok(result.into_iter().next().unwrap_or(Value::Nil))
            }
            Value::Partial(func, partial_args) => {
                let mut all_args = partial_args.clone();
                all_args.extend(args);
                self.call_value_inner(func, all_args, arena)
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
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Nil => String::new(),
        other => other.to_string(),
    }
}

/// 解析 budget 值 (dispatch 层副本,与 execute.rs 同语义)
fn parse_budget_dispatch(v: Value, ctx: &str) -> Result<usize, String> {
    match v {
        Value::Number(n) => {
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
