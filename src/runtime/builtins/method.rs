//! v0.110.0: Value 方法分派（list/dict/builtin/string；数值方法转交 `numeric`）。

use std::collections::HashMap;

use super::*;
use crate::error::{MoraError, MoraResult};
use crate::value::list::List;
use crate::value::{BuiltinKind, Value};

/// Evaluate pure (non-callback, non-ambient) list methods without holding a mutable runtime reference.
pub(crate) fn call_method_list_pure(
    list: List,
    method: &str,
    args: &[Value],
) -> Option<MoraResult<Value>> {
    if matches!(method, "map" | "filter" | "reduce") {
        return None;
    }
    Some(call_method_list_inner(list, method, args))
}

fn call_method_list_inner(list: List, method: &str, args: &[Value]) -> MoraResult<Value> {
    match method {
        "push" => {
            let item = args.first().cloned().unwrap_or(Value::Nil);
            // v0.104.6：List 不可变，push 返回新列表
            let new_list = list.push(item);
            Ok(Value::List(new_list))
        }
        "get" => {
            let index = super::required_num_arg(args, 0, "List.get", "index")?;
            let index = if index < 0.0 {
                return Err(MoraError::Runtime(
                    "List.get: index must be non-negative".to_string(),
                ));
            } else {
                index as usize
            };
            list.get(index).cloned().ok_or_else(|| {
                MoraError::Runtime(format!(
                    "index {} out of bounds (len {})",
                    index,
                    list.len()
                ))
            })
        }
        "pop" => {
            let item = list.pop_last().unwrap_or(Value::Nil);
            Ok(item)
        }
        "len" => Ok(Value::Int(list.len() as i64)),
        // v0.18: take(n) - 取前 n 个元素
        "take" => {
            // v0.104.6 D153：此前只匹配 `Float`，`Int` 实参报「requires a count
            // argument」—— 实参传了，只是个合法的 `Int`。归因错误。
            let n = super::required_num_arg(args, 0, "take", "count")?;
            // v0.104.6 D146：此前 `*n as usize` —— float→int `as` 是**饱和转换**，
            // `-1.0 as usize == 0`，于是 `xs.take(-1)` 得**空列表**（用户以为
            // 「取最后 1 个」），`xs.drop(-1)` 则原样返回全部。两者都 exit 0、
            // 零诊断。同族的 `crush_json(max)` / `tail(max)` 已有非负校验，
            // 此处漏了。
            if n < 0.0 {
                return Err(MoraError::Runtime(format!(
                    "take(): count 不能为负数（得到 {n}）"
                )));
            }
            let result: Vec<Value> = list.into_iter().take(n as usize).collect();
            Ok(Value::List(result.into()))
        }
        // v0.18: drop(n) - 跳过前 n 个元素
        "drop" => {
            // v0.104.6 D153：同 `take` —— 修「Int 被报成没传」的归因错误。
            let n = super::required_num_arg(args, 0, "drop", "count")?;
            if n < 0.0 {
                return Err(MoraError::Runtime(format!(
                    "drop(): count 不能为负数（得到 {n}）"
                )));
            }
            let result: Vec<Value> = list.into_iter().skip(n as usize).collect();
            Ok(Value::List(result.into()))
        }
        // v0.17: window(size) - 滑动窗口
        "window" => {
            // v0.104.6 D153：同 `take` —— 修「Int 被报成没传」的归因错误。
            let size = super::required_num_arg(args, 0, "window", "size")?;
            if size <= 0.0 {
                return Err(MoraError::Runtime("window() size must be > 0".to_string()));
            }
            let size = size as usize;
            let mut windows = Vec::new();
            for i in 0..list.len() {
                if i + size <= list.len() {
                    let window: Vec<Value> = list.slice(i, i + size).to_vec();
                    windows.push(Value::List(window.into()));
                }
            }
            Ok(Value::List(windows.into()))
        }
        // v0.17: batch(size) - 翻转窗口（批次处理）
        "batch" => {
            // v0.104.6 D153：同 `window` —— 修「Int 被报成没传」的归因错误。
            let size = super::required_num_arg(args, 0, "batch", "size")?;
            if size <= 0.0 {
                return Err(MoraError::Runtime("batch() size must be > 0".to_string()));
            }
            let size = size as usize;
            let mut batches = Vec::new();
            for chunk in list.windows(size) {
                batches.push(Value::List(chunk.into()));
            }
            Ok(Value::List(batches.into()))
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
                shape
                    .iter()
                    .map(|n| Value::Int(i64::try_from(*n).unwrap_or(i64::MAX)))
                    .collect(),
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
            Ok(Value::List(result.into()))
        }
        // v0.17: transpose() - 转置二维列表
        "transpose" => {
            if list.is_empty() {
                return Ok(Value::List(vec![].into()));
            }
            // 检查是否是二维列表
            // v0.104.6：`List` 不可变、拿不出连续切片，故这里把每个子列表
            // 降级成 `Vec<Value>`（transpose 本就产出新列表，拷贝可接受）。
            let rows: Vec<Vec<Value>> = list
                .iter()
                .filter_map(|v| {
                    if let Value::List(items) = v {
                        Some(items.to_vec())
                    } else {
                        None
                    }
                })
                .collect();
            if rows.len() != list.len() {
                return Err(MoraError::Runtime(
                    "transpose() requires a 2D list".to_string(),
                ));
            }
            // v0.104.6 D49：此前只校验「每项都是列表」，**不校验各行等长**，
            // 短行用 `row.get(col).unwrap_or(Nil)` 静默补 nil：
            //
            //   [[1,2],[3]].transpose()     → [[1.0, 3.0], [2.0, nil]]
            //   [[1],[2,3]].transpose()     → [[1.0, 2.0], [nil, 3.0]]
            //   [[1,2,3],[4]].transpose()   → [[1.0, 4.0], [2.0, nil], [3.0, nil]]
            //
            // 全部 **exit 0、零提示**，返回一个用 null 补齐的伪矩阵 ——
            // 下游任何按长度/形状算的代码都会拿到垃圾而不自知。
            // 「二维列表」按惯例即各行等长，spec §1109 `.transpose() -> list`
            // 也未承诺补齐语义，故按不规则即报错处理。
            let ncols = rows.first().map(|r| r.len()).unwrap_or(0);
            if rows.iter().any(|r| r.len() != ncols) {
                return Err(MoraError::Runtime(format!(
                    "transpose() requires a rectangular 2D list (row widths differ: {:?})",
                    rows.iter().map(|r| r.len()).collect::<Vec<_>>()
                )));
            }
            let mut result = Vec::new();
            for col in 0..ncols {
                let mut new_row = Vec::new();
                for row in &rows {
                    new_row.push(row.get(col).cloned().ok_or_else(|| {
                        MoraError::Runtime("transpose(): internal row width mismatch".to_string())
                    })?);
                }
                result.push(Value::List(new_row.into()));
            }
            Ok(Value::List(result.into()))
        }
        // v0.17: reshape(rows, cols) - 重塑列表
        "reshape" => {
            // v0.104.6 D153：同 `take` —— 两个实参都修「Int 被报成没传」的归因错误。
            let rows = super::required_num_arg(args, 0, "reshape", "rows")?;
            let cols = super::required_num_arg(args, 1, "reshape", "cols")?;
            if rows < 0.0 || cols < 0.0 {
                return Err(MoraError::Runtime(
                    "reshape(): rows and cols must be non-negative".to_string(),
                ));
            }
            let (rows, cols) = (rows as usize, cols as usize);
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
            // v0.104.6 D325：目标**小于**源 ⇒ 静默丢数据，改为报错。
            //
            // 实测（修前）：`[1,2,3,4,5,6].reshape(1,1)` → `[[1.0]]`，
            // **5 个元素凭空消失，退出码 0、零诊断**。任何在 reshape 结果上
            // 做聚合（`sum` / 循环累加）的代码都会拿到**错误的数**。
            //
            // 为什么这不是「设计」：
            // - `docs/learning-plan.md:53` 写的是「元素按 ravel 顺序复制，
            //   不足则循环重复，但多出元素绝不静默丢弃（报错拦截）」——
            //   方向是「补」，从未说「多就丢」，反而明文禁止静默丢弃；
            // - `docs/mora-spec.md:1110` 只说「重塑列表」；
            // - `tests/list_methods.rs` 的 6 条 reshape 判据全是「恰好」
            //   或「填充」，**无一条覆盖截断**；
            // - numpy / Julia 的 `reshape` 在 size 不匹配时**报错**。
            //
            // 与本文件既有的立场一致（v0.104.6 修 `list.get` 越界静默返回
            // nil 时）：**静默改变数据形状必须是显式错误**。
            // 下方 `while` 只增长不收缩、`flat[r*cols..(r+1)*cols]` 只读前缀，
            // 正是丢数据的直接原因。
            if total < flat.len() {
                return Err(MoraError::Runtime(format!(
                    "reshape(): target shape {}x{} holds {} elements but the input has {} — \
                         reshape() pads but never drops; use take() if you want fewer",
                    rows,
                    cols,
                    total,
                    flat.len()
                )));
            }
            // 循环填充
            while flat.len() < total {
                let extend_len = (total - flat.len()).min(flat.len());
                let extend: Vec<Value> = flat[..extend_len].to_vec();
                flat.extend(extend);
            }
            let mut result = Vec::new();
            for r in 0..rows {
                let row: Vec<Value> = flat[r * cols..(r + 1) * cols].to_vec();
                result.push(Value::List(row.into()));
            }
            Ok(Value::List(result.into()))
        }
        // v0.91: List 统计方法链（list.sum() / .mean() / .min() / .max() / ...）
        // 复用 stats.* builtin 同一底层函数
        "sum" | "mean" | "median" | "stddev" | "var" | "min" | "max" => {
            super::stats::call_stats_method(method, &[Value::List(list.clone())])
        }
        // v0.91 / v0.110.1: List.sort() — 升序排序（支持全数值列表与全字符串列表）
        "sort" => {
            if list.is_empty() {
                return Ok(Value::List(Vec::new().into()));
            }
            if list.iter().all(|v| matches!(v, Value::String(_))) {
                let mut strings: Vec<&str> = list
                    .iter()
                    .filter_map(|v| match v {
                        Value::String(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .collect();
                strings.sort_unstable();
                let sorted: Vec<Value> = strings
                    .into_iter()
                    .map(|s| Value::String(s.to_string()))
                    .collect();
                return Ok(Value::List(sorted.into()));
            }
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
                return Err(MoraError::Runtime(
                    "List.sort: elements must be all numeric or all strings".to_string(),
                ));
            }
            // v0.104.6 D242：用 `total_cmp`（IEEE 754 全序）而非
            // `partial_cmp(...).unwrap_or(Equal)`。
            //
            // 后者遇 NaN 时 `partial_cmp` 返回 `None` → `Equal` ⇒
            // NaN 与一切相等，违反传递性 ⇒ `sort_by` **静默**产出
            // 依赖输入顺序的结果。实测同一组 `[1, NaN, 3, 2]`：
            //   输入 a3b21 顺序 → `[1.0, nan, 2.0, 3.0]`
            //   输入 b31a2 顺序 → `[nan, 1.0, 2.0, 3.0]`
            // NaN 混在中间时「1.0 < NaN < 2.0」是假的排序。
            indexed.sort_by(|a, b| a.1.total_cmp(&b.1));
            // 用 indexed[i].0 取原始 value 重建
            let sorted: Vec<Value> = indexed.iter().map(|(i, _)| list[*i].clone()).collect();
            Ok(Value::List(sorted.into()))
        }
        _ => Err(MoraError::Runtime(format!(
            "List has no method: {}",
            method
        ))),
    }
}

pub(crate) fn call_method_dict_pure(
    map: &HashMap<String, Value>,
    method: &str,
    args: &[Value],
) -> Option<MoraResult<Value>> {
    match method {
        "get" => {
            let key = args.first().map(|v| v.to_string()).unwrap_or_default();
            Some(Ok(map.get(&key).cloned().unwrap_or(Value::Nil)))
        }
        "set" => {
            let key = args.first().map(|v| v.to_string()).unwrap_or_default();
            let value = args.get(1).cloned().unwrap_or(Value::Nil);
            let mut new_map = map.clone();
            new_map.insert(key, value);
            Some(Ok(Value::Dict(new_map)))
        }
        "keys" => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Some(Ok(Value::List(
                keys.into_iter().map(|k| Value::String(k.clone())).collect(),
            )))
        }
        "values" => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            Some(Ok(Value::List(
                entries.into_iter().map(|(_, v)| v.clone()).collect(),
            )))
        }
        "len" => Some(Ok(Value::Int(map.len() as i64))),
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
                return Some(Ok(Value::Dict(err)));
            }
            match json_to_value(&body_str) {
                Ok(val) => {
                    let mut result = HashMap::new();
                    result.insert("ok".to_string(), val);
                    Some(Ok(Value::Dict(result)))
                }
                Err(e) => {
                    let mut err = HashMap::new();
                    err.insert(
                        "err".to_string(),
                        Value::String(format!("ParseError: {}", e.message())),
                    );
                    Some(Ok(Value::Dict(err)))
                }
            }
        }
        _ => {
            if let Some(val) = map.get(method) {
                match val {
                    Value::Task { .. }
                    | Value::Closure { .. }
                    | Value::Continuation(_)
                    | Value::Compose(_)
                    | Value::Partial(_, _)
                    | Value::Curry { .. } => return None,
                    _ => {
                        if args.is_empty() {
                            return Some(Ok(val.clone()));
                        }
                    }
                }
            }
            Some(Err(MoraError::Runtime(format!(
                "Dict has no method: {}",
                method
            ))))
        }
    }
}

pub(crate) fn call_method_builtin_pure(
    kind: BuiltinKind,
    method: &str,
    args: &[Value],
) -> MoraResult<Value> {
    match (kind, method) {
        (BuiltinKind::Json, "parse") => {
            let text = super::required_str_arg(args, 0, "json.parse", "text")?;
            json_to_value(&text).map_err(|e| e.with_prefix("json.parse: "))
        }
        (BuiltinKind::Json, "stringify") => {
            let value = args.first().cloned().unwrap_or(Value::Nil);
            Ok(Value::String(value_to_json(&value)))
        }
        (BuiltinKind::Math, _) => super::math::call_math_method(method, args),
        (BuiltinKind::Stats, _) => super::stats::call_stats_method(method, args),
        (BuiltinKind::Linalg, _) => super::linalg::call_linalg_method(method, args),
        _ => Err(MoraError::Runtime(format!(
            "Unknown method: {:?}.{}",
            kind, method
        ))),
    }
}

pub(crate) fn call_method_string_pure(s: &str, method: &str, args: &[Value]) -> MoraResult<Value> {
    match method {
        // v0.104.6：数**字符**而非 UTF-8 字节
        "len" => Ok(Value::Int(s.chars().count() as i64)),
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
            Ok(Value::List(parts.into()))
        }
        "replace" => {
            let from = args.first().map(|v| v.to_string()).unwrap_or_default();
            let to = args.get(1).map(|v| v.to_string()).unwrap_or_default();
            Ok(Value::String(s.replace(&from, &to)))
        }
        "json" => {
            if s.trim().is_empty() {
                let mut err = HashMap::new();
                err.insert(
                    "err".to_string(),
                    Value::String("ParseError: empty body".to_string()),
                );
                return Ok(Value::Dict(err));
            }
            match json_to_value(s) {
                Ok(val) => {
                    let mut result = HashMap::new();
                    result.insert("ok".to_string(), val);
                    Ok(Value::Dict(result))
                }
                Err(e) => {
                    let mut err = HashMap::new();
                    err.insert(
                        "err".to_string(),
                        Value::String(format!("ParseError: {}", e.message())),
                    );
                    Ok(Value::Dict(err))
                }
            }
        }
        _ => Err(MoraError::Runtime(format!(
            "String has no method: {}",
            method
        ))),
    }
}

/// Evaluate any pure value method; returns `None` when VM callback/ambient/trait dispatch is needed.
pub(crate) fn eval_pure_method(
    object: &Value,
    method: &str,
    args: &[Value],
) -> Option<MoraResult<Value>> {
    match object {
        Value::TraitObject { .. }
        | Value::External(_)
        | Value::Channel(_)
        | Value::TaskHandle(_)
        | Value::Context(_)
        | Value::TcpListener(_)
        | Value::TcpStream(_)
        | Value::TlsListener(_)
        | Value::TlsStream(_)
        | Value::ComputeDag(_)
        | Value::KVAllocator(_)
        | Value::MemoryPool(_)
        | Value::GpuKernel(_)
        | Value::Builtin(BuiltinKind::Random | BuiltinKind::File | BuiltinKind::Exec) => None,
        Value::List(list) => call_method_list_pure(list.clone(), method, args),
        Value::Dict(map) => call_method_dict_pure(map, method, args),
        Value::Builtin(kind) => Some(call_method_builtin_pure(*kind, method, args)),
        Value::String(s) => Some(call_method_string_pure(s, method, args)),
        Value::Int(n) => Some(super::numeric::call_method_numeric(
            &Value::Int(*n),
            method,
            args,
            true,
        )),
        Value::Float(n) => Some(super::numeric::call_method_numeric(
            &Value::Float(*n),
            method,
            args,
            false,
        )),
        Value::Scalar(s) => Some(super::numeric::call_method_numeric(
            &Value::Scalar(*s),
            method,
            args,
            s.dtype().is_int(),
        )),
        Value::Tensor(t) => Some(crate::value::tensor::call_tensor_method(t, method, args)),
        _ => Some(Err(MoraError::Runtime(
            "Can only call methods on lists, dicts, strings, numbers, or builtin objects"
                .to_string(),
        ))),
    }
}
