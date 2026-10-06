//! v0.75.51: plan.* builtin 实现 — 从 builtins/mod.rs 拆出（P7，
//! Rhai register_plugin/Koto workspace 思想：按 domain 拆分，mod.rs 仅
//! 聚合）。方法语义与拆分前完全一致。

use super::*;
use crate::value::Value;

impl Interpreter {
    pub fn call_plan_method(&mut self, method: &str, args: &[Value]) -> Result<Value, String> {
        let mut plans = self.orch.plans.lock().expect("plans poisoned");

        match method {
            "create" => {
                if args.len() < 2 {
                    return Err("plan.create: requires 2 args (name, steps)".to_string());
                }
                let name = args[0].to_string();
                let steps_arg = match &args[1] {
                    Value::List(items) => items,
                    _ => {
                        return Err(
                            "plan.create: steps must be a list of {id, text} dicts".to_string()
                        );
                    }
                };
                let mut plan = crate::plan::Plan::new();
                for (i, s) in steps_arg.iter().enumerate() {
                    let d = match s {
                        Value::Dict(d) => d,
                        _ => return Err(format!("plan.create: steps[{}] must be a dict", i)),
                    };
                    let id = match d.get("id") {
                        Some(Value::String(s)) => s.clone(),
                        _ => return Err(format!("plan.create: steps[{}].id must be a string", i)),
                    };
                    let text = match d.get("text") {
                        Some(Value::String(s)) => s.clone(),
                        _ => {
                            return Err(format!("plan.create: steps[{}].text must be a string", i));
                        }
                    };
                    // v0.104.6 D343：此前是
                    //   `StepStatus::parse(s).unwrap_or(StepStatus::Pending)`
                    // ⇒ **非法 status 静默变成 pending**，exit 0、零诊断。实测：
                    //   plan.create("p1", [{id:"s1",text:"t1",status:"done"},
                    //                          {id:"s2",text:"t2",status:"BOGUS"}])
                    //   → s1 = done ✅，s2 = **pending ⬜**（拼写错误被吞掉）
                    //
                    // **同一模块内两种策略并存**：`plan.update`（下面第 87 行）对
                    // 同样的非法值用 `.ok_or_else(...)` **明确报错**。
                    // `StepStatus::parse` 返回 `Option` 就是在说「可能不合法」，
                    // 调用方把这个信息丢掉是**漏写**，不是设计。
                    //
                    // 危害：`create` 出来的计划里，某一步**看起来是待办**，
                    // 于是依赖 `done` 的下游逻辑（完成度统计 / 收尾检查）漏掉它，
                    // 而用户完全不知道自己拼错了状态名。
                    let status = match d.get("status") {
                        Some(Value::String(s)) => {
                            crate::plan::StepStatus::parse(s).ok_or_else(|| {
                                format!(
                                    "plan.create: steps[{}].status invalid '{}' \
                                     (expected one of: pending/todo/⬜, \
                                     in_progress/in-progress/doing/🔄, \
                                     done/completed/finish/✅)",
                                    i, s
                                )
                            })?
                        }
                        _ => crate::plan::StepStatus::Pending,
                    };
                    plan.add_step(crate::plan::PlanStep::new(id, text).with_status(status))
                        .map_err(|e| format!("plan.create: {}", e))?;
                }
                plans.insert(name.clone(), plan);
                Ok(Value::String(name))
            }
            "update" => {
                if args.len() < 2 {
                    return Err("plan.update: requires 2 args (name, updates)".to_string());
                }
                let name = args[0].to_string();
                let updates = match &args[1] {
                    Value::List(items) => items,
                    _ => {
                        return Err(
                            "plan.update: updates must be a list of [id, status]".to_string()
                        );
                    }
                };
                let mut parsed_updates: Vec<(String, crate::plan::StepStatus)> = Vec::new();
                for (i, u) in updates.iter().enumerate() {
                    let pair = match u {
                        Value::List(p) if p.len() == 2 => p,
                        _ => {
                            return Err(format!(
                                "plan.update: updates[{}] must be [id, status]",
                                i
                            ));
                        }
                    };
                    let id = match &pair[0] {
                        Value::String(s) => s.clone(),
                        _ => {
                            return Err(format!(
                                "plan.update: updates[{}][0] must be id string",
                                i
                            ));
                        }
                    };
                    let status = match &pair[1] {
                        Value::String(s) => crate::plan::StepStatus::parse(s).ok_or_else(|| {
                            format!("plan.update: updates[{}][1] invalid status '{}'", i, s)
                        })?,
                        _ => {
                            return Err(format!(
                                "plan.update: updates[{}][1] must be status string",
                                i
                            ));
                        }
                    };
                    parsed_updates.push((id, status));
                }
                let plan = plans
                    .get_mut(&name)
                    .ok_or_else(|| format!("plan.update: plan '{}' not found", name))?;
                plan.update(&parsed_updates)
                    .map_err(|e| format!("plan.update: {}", e))?;
                Ok(Value::Bool(true))
            }
            "add" => {
                if args.len() < 3 {
                    return Err("plan.add: requires 3 args (name, id, text)".to_string());
                }
                let name = args[0].to_string();
                let id = args[1].to_string();
                let text = args[2].to_string();
                let plan = plans
                    .get_mut(&name)
                    .ok_or_else(|| format!("plan.add: plan '{}' not found", name))?;
                plan.add_step(crate::plan::PlanStep::new(id, text))
                    .map_err(|e| format!("plan.add: {}", e))?;
                Ok(Value::Bool(true))
            }
            "remove" => {
                if args.len() < 2 {
                    return Err("plan.remove: requires 2 args (name, id)".to_string());
                }
                let name = args[0].to_string();
                let id = args[1].to_string();
                let plan = plans
                    .get_mut(&name)
                    .ok_or_else(|| format!("plan.remove: plan '{}' not found", name))?;
                let removed = plan.remove_step(&id);
                Ok(Value::Bool(removed.is_some()))
            }
            "list" => {
                // v0.104.6 D152：此前写成
                //   `if let Some(Value::String(name)) = args.first() { 查该计划 } else { 列所有计划名 }`
                // 而这里的 `else` **不是默认值、是另一个操作** —— 于是传错类型的 name
                // （`plan.list(5)` / `plan.list(["alpha"])`）会静默返回**所有计划名**，
                // exit 0、零诊断。用户要某计划的步骤，拿到的是另一件事，
                // 下游 `filter` / 下标随之给出错误结果却无任何报错。
                //
                // 同文件的 `create` / `update` 对每个字段都有精确类型报错
                // （`steps[{}].id must be a string` 等）—— 即「同族里有人做对了」。
                if let Some(name) = optional_str_arg(args, 0, "plan.list", "plan name")? {
                    let plan = plans
                        .get(&name)
                        .ok_or_else(|| format!("plan.list: plan '{}' not found", name))?;
                    let items: Vec<Value> = plan
                        .steps()
                        .iter()
                        .map(|s| {
                            let mut d = std::collections::HashMap::new();
                            d.insert("id".to_string(), Value::String(s.id.clone()));
                            d.insert("text".to_string(), Value::String(s.text.clone()));
                            d.insert(
                                "status".to_string(),
                                Value::String(s.status.as_str().to_string()),
                            );
                            d.insert(
                                "emoji".to_string(),
                                Value::String(s.status.emoji().to_string()),
                            );
                            Value::Dict(d)
                        })
                        .collect();
                    Ok(Value::List(items.into()))
                } else {
                    let mut names: Vec<String> = plans.keys().cloned().collect();
                    names.sort();
                    Ok(Value::List(names.into_iter().map(Value::String).collect()))
                }
            }
            "info" => {
                let name = args
                    .first()
                    .ok_or("plan.info: requires plan name")?
                    .to_string();
                let plan = plans
                    .get(&name)
                    .ok_or_else(|| format!("plan.info: plan '{}' not found", name))?;
                let mut d = std::collections::HashMap::new();
                d.insert("name".to_string(), Value::String(name));
                d.insert("total".to_string(), Value::Float(plan.len() as f64));
                d.insert(
                    "done".to_string(),
                    Value::Float(plan.complete_count() as f64),
                );
                d.insert(
                    "pending".to_string(),
                    Value::Float(plan.pending_count() as f64),
                );
                d.insert(
                    "completion_ratio".to_string(),
                    Value::Float(plan.completion_ratio()),
                );
                Ok(Value::Dict(d))
            }
            _ => Err(format!("plan.{}: unknown method", method)),
        }
    }
}
