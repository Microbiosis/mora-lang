//! v0.92: `Interpreter::call_method*` — Value 方法调用分派（自 dispatch.rs 迁出，P1.2 拆分）。
//!
//! dispatch.rs 的 `impl Interpreter` 是 P0 时期遗留的巨型 block。
//! 这里按 concern 拆出「Value 方法分派」（list/dict/builtin/conversation/
//! string/stream/agent/router/mcp/document）；dispatch.rs 保留
//! `call_function`（builtin 入口）+ `call_builtin_*` + `call_value`。

use parking_lot::Mutex;

use super::*;
use crate::common::Span;
use crate::value::{BuiltinKind, Value};
use super::dispatch::block_on_async;

impl Interpreter {
    /// v0.17: 直接调用 Value 形式的函数（用于管道闭包）
    pub(super) fn call_method(
        &mut self,
        object: Value,
        method: &str,
        args: Vec<Value>,
        call_site: Span,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        // v0.35: inline-cache 已删；TraitObject 走 dispatch_trait_method
        if let Value::TraitObject { .. } = &object {
            return self.dispatch_trait_method(&object, method, args, call_site, effects);
        }
        match object {
            Value::List(list) => self.call_method_list(list, method, args, effects),
            Value::Dict(map) => self.call_method_dict(map, method, args, effects),
            // v0.99: random.* 方法调用 = ambient effect perform（原进程级
            // 全局 Mutex 状态机数据流化；状态在 CoreRuntime.random_state，
            // 类型层效果行 + 签名见 typeck 的 ambient 预置）。
            Value::Builtin(BuiltinKind::Random) => self.call_random_ambient(method, args, effects),
            Value::Builtin(kind) => self.call_method_builtin(kind, method, args),
            Value::Conversation { .. } => self.call_method_conversation(object, method, args),
            Value::String(s) => self.call_method_string(s, method, args),
            Value::Stream { reader, done, xform } => {
                self.call_method_stream(reader, done, xform, method, args)
            }
            Value::Agent { .. } => self.call_method_agent(object, method, args),
            Value::Router { routes } => self.call_method_router(routes, method, args),
            Value::McpServer { tools } => self.call_method_mcp(tools, method, args),
            Value::Document { backend, .. } => self.call_method_document(backend.as_ref(), method),
            // v0.91: 数值方法链（x.abs() / x.sqrt() 等）
            // 复用 math 模块避免双实现（同一底层函数）
            Value::Int(n) => super::numeric_helpers::call_method_numeric(&Value::Int(n), method, &args, true),
            Value::Float(n) => super::numeric_helpers::call_method_numeric(&Value::Float(n), method, &args, false),
            Value::BigInt(n) => super::numeric_helpers::call_method_bigint(&Value::BigInt(n), method, &args),
            _ => Err("Can only call methods on lists, dicts, strings, conversations, streams, agents, routers, mcp_servers, documents, or builtin objects".to_string()),
        }
    }

    /// v0.99: `random.*` 方法调用 → ambient effect perform。
    ///
    /// 方法名映射为 ambient 标签（`crate::mir::effect::ambient`，与 typeck
    /// 的效果行标签/签名预置同源），运行时查找顺序：用户 handle 注册表 →
    /// `CoreRuntime.random_state` 兜底。旧 `call_random_method`（进程级
    /// 全局 Mutex 状态）已删除 —— 状态单属主化后本方法只是标签翻译层。
    fn call_random_ambient(
        &mut self,
        method: &str,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
    ) -> Result<Value, String> {
        let label = match crate::mir::effect::ambient::random_label_for_method(method) {
            Some(l) => l,
            None => return Err(format!("random.{}: unknown method", method)),
        };
        match crate::mir::host::MirHost::perform_effect(self, label, args, effects) {
            Some(v) => Ok(v),
            None => Err(format!(
                "unhandled effect: {} (ambient random state missing — runtime invariant violated)",
                label
            )),
        }
    }

    fn call_method_list(
        &mut self,
        list: Vec<Value>,
        method: &str,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
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
                    let mapped = self.call_value(&mapper, vec![item], effects)?;
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
                    let keep = self.call_value(&predicate, vec![item.clone()], effects)?;
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
                    acc = self.call_value(&reducer, vec![acc, item], effects)?;
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
            // v0.91: List 统计方法链（list.sum() / .mean() / .min() / .max() / ...）
            // 复用 stats.* builtin 同一底层函数
            "sum" | "mean" | "median" | "stddev" | "var" | "min" | "max" => {
                crate::interpreter::builtins::stats::call_stats_method(method, &[Value::List(list.clone())])
            }
            // v0.91: List.sort() — 升序排序（仅数值列表，非数值原样返回）
            "sort" => {
                // 提取 f64 副本用于排序
                let mut indexed: Vec<(usize, f64)> = list
                    .iter()
                    .enumerate()
                    .filter_map(|(i, v)| match v {
                        Value::Int(n) => Some((i, *n as f64)),
                        Value::Float(n) => Some((i, *n)),
                        _ => None,
                    })
                    .collect();
                if indexed.len() != list.len() {
                    return Err("List.sort: contains non-numeric elements".to_string());
                }
                indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                // 用 indexed[i].0 取原始 value 重建
                let sorted: Vec<Value> = indexed.iter().map(|(i, _)| list[*i].clone()).collect();
                Ok(Value::List(sorted))
            }
            _ => Err(format!("List has no method: {}", method)),
        }
    }
    fn call_method_dict(
        &mut self,
        map: HashMap<String, Value>,
        method: &str,
        args: Vec<Value>,
        effects: &mut crate::mir::effect::Effects,
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
                            return self.call_value(val, args, effects);
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
            // v0.75.84: ai.chat(prompt[, {model: "..."}]) — 补回运行时 dispatch arm。
            // v0.37 typed BuiltinKind 重构后此 arm 缺失，`ai.chat(...)` 落
            // `_ => Err("Unknown method")`；MoA 的每层多模型并行依赖它。
            // model 可选：dict 参数 {model} 优先于 env（MORA_AI_MODEL）。
            (BuiltinKind::AiChat, "chat") => {
                let prompt = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => return Err("ai.chat requires a prompt argument".to_string()),
                };
                let model = match args.get(1) {
                    Some(Value::Dict(d)) => match d.get("model") {
                        Some(Value::String(m)) => m.clone(),
                        _ => std::env::var(AI_MODEL_ENV)
                            .unwrap_or_else(|_| AI_MODEL_DEFAULT.to_string()),
                    },
                    _ => {
                        std::env::var(AI_MODEL_ENV).unwrap_or_else(|_| AI_MODEL_DEFAULT.to_string())
                    }
                };
                Self::do_ai_chat(self, &model, &prompt)
            }
            (BuiltinKind::AiChat, "critic") => {
                // v0.103: ai.critic(answer, ctx?) —— spec §12.5 `string, string? -> value`。
                // 用 AI 对给定 answer 做批判性评估，返回结构化裁决：
                //   {verdict: "pass"|"fail", critique: <文本>, score: 0..1}
                // 评估走与 ai.chat 同一模型通道（mock 模式下返回确定性裁决），
                // 使「评估输出」这条 spec 承诺有真实运行时实现而非 Unknown 方法。
                let answer = match args.first() {
                    Some(Value::String(s)) => s.clone(),
                    Some(other) => other.to_string(),
                    None => return Err("ai.critic requires an answer argument".to_string()),
                };
                let ctx = match args.get(1) {
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                };
                let prompt = if ctx.is_empty() {
                    format!(
                        "Critically evaluate the following answer. Reply with a single \
                         word verdict (PASS or FAIL) on the first line, then a \
                         one-sentence critique.\n\nAnswer:\n{}",
                        answer
                    )
                } else {
                    format!(
                        "Critically evaluate the following answer against the given \
                         context. Reply with a single word verdict (PASS or FAIL) on \
                         the first line, then a one-sentence critique.\n\nContext:\n{}\n\nAnswer:\n{}",
                        ctx, answer
                    )
                };
                let model =
                    std::env::var(AI_MODEL_ENV).unwrap_or_else(|_| AI_MODEL_DEFAULT.to_string());
                let reply = Self::do_ai_chat(self, &model, &prompt)?;
                let critique = reply.to_string();
                // 裁决：只看**首行首个词**是否为 FAIL/PASS。批判正文里出现
                // 这些词不影响判定；mock 模式回显的指令文本不含裸 verdict 行，
                // 故缺省按 pass（mock 不是真实评估，不应系统性误报 fail）。
                let head_word = critique
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .and_then(|l| l.split_whitespace().next())
                    .unwrap_or("")
                    .trim_matches(|c: char| !c.is_ascii_alphabetic())
                    .to_uppercase();
                let verdict = if head_word == "FAIL" { "fail" } else { "pass" };
                let score = if verdict == "pass" { 1.0 } else { 0.0 };
                let mut out = std::collections::HashMap::new();
                out.insert("verdict".to_string(), Value::String(verdict.to_string()));
                out.insert("critique".to_string(), Value::String(critique));
                out.insert("score".to_string(), Value::Float(score));
                Ok(Value::Dict(out))
            }
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
            // v0.83: TEA runtime 与 transducer builtin dispatch
            (BuiltinKind::Tea, _) => self.call_tea_method(method, &args),
            (BuiltinKind::Xform, _) => self.call_xform_method(method, &args),
            // v0.45.0: ai.retry / ai.role — top-level AI utilities
            // (chat still handled by existing AiChat dispatch below)
            (BuiltinKind::Ai, _) => self.call_ai_method(method, &args),
            // v0.91: math/stats/linalg/random 四件套
            (BuiltinKind::Math, _) => {
                crate::interpreter::builtins::math::call_math_method(method, &args)
            }
            (BuiltinKind::Stats, _) => {
                crate::interpreter::builtins::stats::call_stats_method(method, &args)
            }
            (BuiltinKind::Linalg, _) => {
                crate::interpreter::builtins::linalg::call_linalg_method(method, &args)
            }
            // v0.99: BuiltinKind::Random 不再走本表 —— call_method 在进入
            // 前已拦截并转 ambient effect perform（call_random_ambient）。
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
                .map_err(|e| e.to_string())
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
        xform: Option<Arc<dyn crate::value::transducer::Transducer<String, String>>>,
        method: &str,
        _args: Vec<Value>,
    ) -> Result<Value, String> {
        match method {
            "collect" => {
                let mut result = String::new();
                if !*done.lock() {
                    let mut guard = reader.lock();
                    // v0.83: apply transducer to each raw SSE token.
                    // Arc::get_mut requires unique ownership; if shared
                    // (other holder exists), fall back to identity.
                    let mut xform_arc = xform;
                    loop {
                        let xform_mut: Option<&mut dyn crate::value::transducer::Transducer<String, String>> =
                            xform_arc.as_mut().and_then(|arc| {
                                std::sync::Arc::get_mut(arc).map(|t| { t as &mut dyn crate::value::transducer::Transducer<String, String> })
                            });
                        match Self::read_next_sse_token(&mut guard, xform_mut) {
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
}
