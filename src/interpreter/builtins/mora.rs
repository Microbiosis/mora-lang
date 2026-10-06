//! v0.75.51: mora.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_mora_method(&mut self, method: &str, args: &[Value]) -> Result<Value, String> {
        match method {
            "refine" => {
                if args.len() < 2 {
                    return Err(
                        "mora.refine: requires 2 args (script_path, instruction)".to_string()
                    );
                }
                let script = std::path::PathBuf::from(args[0].to_string());
                let instruction = args[1].to_string();
                // v0.75.8: 可选第 3 参 count — 生成 N 个候选副本（多方案生成）。
                // 2 参（count=1）返回单个 Dict（行为不变）；3 参返回 List[Dict]。
                // v0.104.6 D150：此前只匹配 `Value::Float`，于是**合法的 `Int`**
                // 被静默丢弃、count 退回 1 —— 而 count 决定**返回类型**
                // （见下方 `if count == 1`）：传 Int 会让 `List[Dict]` 静默变成
                // 单个 `Dict`，调用方的 `for` / 下标随之失效。
                let count = optional_num_arg(args, 2, "mora.refine", "count")?
                    .map(|n| n as usize)
                    .unwrap_or(1);
                // v0.49.0 (A2): drop lock before file I/O.
                // get_or_create 只创建空 session (无 I/O); refine 是 I/O 在锁外
                let steps = {
                    let mut registry = self
                        .orch
                        .refine_registry
                        .lock()
                        .expect("refine_registry poisoned");
                    let session = registry.get_or_create(&script);
                    session.refine_many(&instruction, count)
                }
                .map_err(|e| format!("mora.refine: {}", e))?;
                if count == 1 {
                    Ok(Value::Dict(
                        steps
                            .into_iter()
                            .next()
                            .expect("mora.refine: refine_many(1) returned no step")
                            .to_dict(),
                    ))
                } else {
                    Ok(Value::List(
                        steps
                            .into_iter()
                            .map(|s| Value::Dict(s.to_dict()))
                            .collect(),
                    ))
                }
            }
            "refine_info" => {
                if args.is_empty() {
                    return Err("mora.refine_info: requires script_path".to_string());
                }
                let script = std::path::PathBuf::from(args[0].to_string());
                // v0.104.6 D150：此前只匹配 `Value::Float`，于是**合法的 `Int`**
                // 被静默丢弃、`iter` 变 `None` → 走 `latest_step()` 分支 ——
                // 即「查第 1 轮」静默返回**最新一轮**（exit 0、零诊断）。
                let iter =
                    optional_num_arg(args, 1, "mora.refine_info", "iteration")?.map(|n| n as usize);
                let registry = self
                    .orch
                    .refine_registry
                    .lock()
                    .expect("refine_registry poisoned");
                let session = registry.get(&script).ok_or_else(|| {
                    format!("mora.refine_info: no session for '{}'", script.display())
                })?;
                let step = if let Some(n) = iter {
                    session
                        .steps
                        .get(n.saturating_sub(1))
                        .ok_or_else(|| format!("mora.refine_info: iteration {} not found", n))?
                } else {
                    session
                        .latest_step()
                        .ok_or_else(|| "mora.refine_info: no steps yet".to_string())?
                };
                Ok(Value::Dict(step.to_dict()))
            }
            "list_refines" => {
                let registry = self
                    .orch
                    .refine_registry
                    .lock()
                    .expect("refine_registry poisoned");
                let mut names: Vec<String> = Vec::new();
                for path in registry.session_paths() {
                    names.push(path.clone());
                }
                names.sort();
                Ok(Value::List(names.into_iter().map(Value::String).collect()))
            }
            _ => Err(format!("mora.{}: unknown method", method)),
        }
    }
}
