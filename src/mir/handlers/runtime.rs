//! Runtime instructions — orchestrate, file I/O, and private execution helpers.

use std::cmp::Ordering;
use std::collections::HashMap;

use crate::mir::host::MirHost;

use crate::mir::vm::run_mir;

use crate::mir::{MirFunction, Reg};

use crate::value::{Environment, Value};

use crate::mir::vm::value_to_string;

// ============================================================
// Private helpers
// ============================================================

/// v0.68: Unified isolated-block execution.
///
/// Clones `env`, runs `body` in the clone, then merges the child's changes
/// back into the parent using the interpreter's current merge strategies.
/// Returns the body's final value AND any merge conflicts (currently
/// discarded by callers; reserved for future observability hooks).
pub(super) fn run_isolated(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(crate::value::Value, Vec<crate::value::Conflict>), String> {
    let mut child_env = env.clone();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let result = run_mir(&std::sync::Arc::new((*body).clone()), interp, &mut child_env, effects)?;
    let strategies = interp.current_merge_strategies();
    let conflicts = match strategies.as_ref() {
        Some(s) => env.merge_from_with_strategies(
            &child_env,
            s,
            &crate::value::MergeStrategy::LastWriteWins,
        ),
        None => {
            env.merge_from(&child_env, &crate::value::MergeStrategy::LastWriteWins);
            Vec::new()
        }
    };
    Ok((result, conflicts))
}

/// 执行预 lowered MirFunction → Value（零 MirExpr 依赖）。
/// v0.91: 替代 eval_expr_value，handlers 不再需要 MirExpr 做运行时求值。
pub(super) fn run_mir_fn(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    func: &MirFunction,
    effects: &mut crate::mir::effect::Effects,
) -> Result<Value, String> {
    let mut inner_env = env.clone();
    crate::mir::vm::run_mir(&std::sync::Arc::new(func.clone()), interp, &mut inner_env, effects)
}

/// Value → f64（router 分数解析；Int/Float 均接受）。
pub(super) fn value_to_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

// ============================================================
// Runtime / Transaction / Isolation instructions
// ============================================================

pub fn h_transaction(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
    compensation: &MirFunction,
    effects: &mut crate::mir::effect::Effects,
) -> Result<super::Flow, String> {
    match run_isolated(interp, env, body, effects) {
        Ok(_) => Ok(super::Flow::Continue),
        Err(_) => {
            let mut comp_env = env.clone();
            // v0.75.9: 包裹 Arc 走全局 DAG 缓存
            if let Err(e) = run_mir(&std::sync::Arc::new((*compensation).clone()), interp, &mut comp_env, effects) {
                eprintln!("[warn] transaction compensation failed: {}", e);
            }
            Err("Transaction rolled back".to_string())
        }
    }
}

pub fn h_worker(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let _ = run_isolated(interp, env, body, effects)?;
    Ok(())
}

pub fn h_observe(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    // v0.68: Bug fix — was discarding child_env mutations. Now merges
    // via run_isolated so observability side-effects (trace vars, span
    // markers) are actually visible.
    let _ = run_isolated(interp, env, body, effects)?;
    Ok(())
}

pub fn h_span(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    // v0.68: Bug fix — same as h_observe.
    let _ = run_isolated(interp, env, body, effects)?;
    Ok(())
}

pub fn h_prompt_section(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let mut child_env = env.clone();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let _ = run_mir(&std::sync::Arc::new((*body).clone()), interp, &mut child_env, effects);
    Ok(())
}

pub fn h_document_section(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    body: &MirFunction,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let mut child_env = env.clone();
    // v0.75.9: 包裹 Arc 走全局 DAG 缓存
    let _ = run_mir(&std::sync::Arc::new((*body).clone()), interp, &mut child_env, effects);
    Ok(())
}

// ============================================================
// File I/O handlers
// ============================================================

pub fn h_save(
    interp: &mut dyn MirHost,
    env: &Environment,
    regs: &[Value],
    path: Reg,
    value: Reg,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let value_str = value_to_string(&regs[value]);
    interp.mir_call_function(
        "file.write_text",
        vec![Value::String(path_str), Value::String(value_str)],
        env,
        effects,
    )?;
    Ok(())
}

pub fn h_load(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &[Value],
    path: Reg,
    var: &str,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content = interp.mir_call_function("file.read_text", vec![Value::String(path_str)], env, effects)?;
    env.define(var.to_string(), content, false);
    Ok(())
}

pub fn h_read_file(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &[Value],
    path: Reg,
    var: &str,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content = interp.mir_call_function("file.read_text", vec![Value::String(path_str)], env, effects)?;
    env.define(var.to_string(), content, false);
    Ok(())
}

pub fn h_write_file(
    interp: &mut dyn MirHost,
    env: &Environment,
    regs: &[Value],
    path: Reg,
    content: Reg,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content_str = value_to_string(&regs[content]);
    interp.mir_call_function(
        "file.write_text",
        vec![Value::String(path_str), Value::String(content_str)],
        env,
        effects,
    )?;
    Ok(())
}

pub fn h_append_file(
    interp: &mut dyn MirHost,
    env: &Environment,
    regs: &[Value],
    path: Reg,
    content: Reg,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content_str = value_to_string(&regs[content]);
    interp.mir_call_function(
        "file.append_text",
        vec![Value::String(path_str), Value::String(content_str)],
        env,
        effects,
    )?;
    Ok(())
}

pub fn h_read_bytes_file(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    regs: &[Value],
    path: Reg,
    var: &str,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let bytes = interp.mir_call_function("file.read_bytes", vec![Value::String(path_str)], env, effects)?;
    env.define(var.to_string(), bytes, false);
    Ok(())
}

pub fn h_write_bytes_file(
    interp: &mut dyn MirHost,
    env: &Environment,
    regs: &[Value],
    path: Reg,
    content: Reg,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    let path_str = value_to_string(&regs[path]);
    let content_val = regs[content].clone();
    interp.mir_call_function(
        "file.write_bytes",
        vec![Value::String(path_str), content_val],
        env,
        effects,
    )?;
    Ok(())
}

// ============================================================
// Orchestrate / Eval
// ============================================================

/// v0.93: **纯生产者** —— 返回 `Effect` 值而非改写宿主状态。
/// 调用方（dispatch）把返回值 push 进显式 `Effects` 累加器。
pub fn h_send(
    interp: &mut dyn MirHost,
    regs: &[Value],
    value: Reg,
    target: &str,
) -> crate::mir::effect::Effect {
    let val = regs[value].clone();
    // v0.83: 录制 Msg 事件 — 发送 BSP 消息本身也是 TEA-style 应用层事件。
    // （recorder 是 TEA 录制通道，与 BSP 效应累加器正交。）
    if let Some(rec) = interp.recorder_mut() {
        // prior_state_hash 用 sys time 简化（完整实现见 src/tea/replay.rs）
        rec.record_msg(target.to_string(), val.clone(), 0);
    }
    // v0.70: Removed crossbeam worker_channels fallback (was dead code).
    crate::mir::effect::Effect::Send(crate::checkpoint::SendTask {
        target_node: target.to_string(),
        input: val,
    })
}

/// v0.71: Contribute a value to a per-super-step aggregator.
/// Currently a no-op when no Pregel run is active (aggregators are BSP-only).
/// v0.93: **纯生产者** —— 返回 `Effect` 值而非改写宿主状态。
/// 与 h_send 同构：此前 push 到宿主可变缓冲，导致并行 worker 克隆宿主的
/// 贡献被静默丢弃（正确性缺陷）；现改为返回值，由 dispatch 折叠进显式
/// `Effects`，worker 边界统一 merge。
pub fn h_aggregate(regs: &[Value], value: Reg, name: &str) -> crate::mir::effect::Effect {
    crate::mir::effect::Effect::Contribute(crate::mir::orchestrate::AggregatorContribution {
        name: name.to_string(),
        value: regs[value].clone(),
    })
}

pub fn h_eval(
    regs: &[Value],
    env: &mut Environment,
    name: &str,
    given_reg: Reg,
    expects: &[Reg],
    tolerance: &Option<f64>,
) -> Result<(), String> {
    let given_val = regs[given_reg].clone();
    env.define("given".to_string(), given_val.clone(), false);
    for &expect_reg in expects {
        let expect_val = regs[expect_reg].clone();
        let pass = if let Some(tol) = tolerance {
            match (&given_val, &expect_val) {
                (Value::Float(g), Value::Float(e)) => (g - e).abs() <= *tol,
                (Value::Int(g), Value::Int(e)) => (*g as f64 - *e as f64).abs() <= *tol,
                _ => given_val == expect_val,
            }
        } else {
            given_val == expect_val
        };
        if !pass {
            return Err(format!(
                "eval '{}': assertion failed: given {:?}, expected {:?}",
                name, given_val, expect_val
            ));
        }
    }
    eprintln!("eval '{}': PASSED", name);
    Ok(())
}

// ============================================================
// Orchestrate
// ============================================================

pub fn h_orchestrate(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    input_var: &str,
    result_var: &str,
    kind: &crate::mir::orchestrate::MirOrchestrateKind,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    match kind {
        crate::mir::orchestrate::MirOrchestrateKind::Pregel {
            agents,
            edges,
            state_schema,
            checkpoint,
            interrupt_points,
            adjacency,
        } => {
            let config = crate::mir::orchestrate::MirPregelConfig {
                agents: agents.clone(),
                edges: edges.clone(),
                state_schema: state_schema.clone(),
                checkpoint: checkpoint.clone(),
                interrupt_points: interrupt_points.clone(),
                adjacency: adjacency.clone(),
                aggregators: Vec::new(),
                master_compute: None,
            };
            run_pregel_config(interp, env, config, input_var, result_var, effects)
        }
        // v0.75.84: MoA（Mixture-of-Agents，arXiv:2406.04692）— 展开为
        // pregel 图：每层 L = [N 个 proposer 并行 ai.chat] → [聚合 agent
        // 综合]。proposer 结果经 aggregate（Concat）提交，聚合 agent 读
        // input_aggregator_layer_{L}_responses 综合；聚合结果写 result
        // channel，下一层 proposer 读上一层的 responses channel 继续。
        // 复用 v0.75.83 aggregate 缓冲通道 + pregel BSP，零新引擎机制。
        crate::mir::orchestrate::MirOrchestrateKind::Moa {
            layers,
            proposers,
            aggregator,
            prompt: _,
            prompt_fn,
            ..
        } => {
            let config = build_moa_config(*layers, proposers, aggregator, prompt_fn, input_var)?;
            run_pregel_config(interp, env, config, input_var, result_var, effects)
        }
        // v0.75.85: MoE（Mixture-of-Experts，Shazeer 2017 稀疏门控）— 单轮
        // 线性流程：router 打分 → top-k 稀疏激活 → 专家执行 → 加权组合。
        // 顺序执行（MoE 无超步，不用 pregel 图）。
        // v0.91: 使用预 lowering 的 router_fn/prompt_fn，消除 MirExpr 跨层依赖。
        crate::mir::orchestrate::MirOrchestrateKind::Moe {
            experts,
            router: _,
            top_k,
            prompt: _,
            router_fn,
            prompt_fn,
        } => run_moe(
            interp,
            env,
            experts,
            router_fn,
            *top_k,
            prompt_fn,
            input_var,
            result_var,
            effects,
        ),
        crate::mir::orchestrate::MirOrchestrateKind::Sequential { agents } => {
            // v0.75.34: Sequential orchestrate 执行 — 按声明顺序逐个执行
            // agent 的 prelowered task_body，前一个 agent 的输出作为下一个
            // 的输入（pipeline），最终结果写入 result_var。
            // 输入注入沿用 pregel 契约：`input` 变量（input_var 的当前值）。
            let mut input_val = env.get(input_var).unwrap_or(Value::Nil);
            let mut result = Value::Nil;
            for agent in agents {
                if agent.task_body.body.is_empty() && agent.task_body.n_regs == 0 {
                    return Err(format!(
                        "orchestrate: agent '{}' has empty task_body (lowering missing)",
                        agent.name
                    ));
                }
                // 每 agent 独立 env（克隆父级）：input 定义在私有副本上，
                // 避免跨 agent 污染；副作用写回见下方合并。
                let mut agent_env = env.clone();
                agent_env.define("input".to_string(), input_val.clone(), false);
                agent_env.clock.tick(&agent.name);
                result = crate::mir::vm::run_mir(
                    &std::sync::Arc::new(agent.task_body.clone()),
                    interp,
                    &mut agent_env,
                    effects,
                )?;
                // agent 期间 define 的变量合并回父 env（与 pregel 引擎
                // reconcile_outcome 的写回语义一致）。
                for (name, val) in agent_env.iter() {
                    if env.get(&name).is_none() {
                        env.define(name, val, false);
                    }
                }
                input_val = result.clone();
            }
            env.define(result_var.to_string(), result, false);
            Ok(())
        }
        // v0.92: Graph orchestrate — 图执行。Graph 语义即 pregel 图（agents
        // 为顶点、edges 为静态边），直接映射到 pregel 配置复用 BSP 引擎：
        // 每超步按边传播消息、顶点 task_body 执行、vote_to_halt 收敛。
        // 与 Pregel 变体的唯一区别：无 state_schema/checkpoint/interrupt。
        crate::mir::orchestrate::MirOrchestrateKind::Graph { agents, edges } => {
            let config = crate::mir::orchestrate::MirPregelConfig {
                agents: agents.clone(),
                edges: edges.clone(),
                state_schema: Vec::new(),
                checkpoint: None,
                interrupt_points: Vec::new(),
                adjacency: HashMap::new(),
                aggregators: Vec::new(),
                master_compute: None,
            };
            run_pregel_config(interp, env, config, input_var, result_var, effects)
        }
        // v0.92: Loop orchestrate — 迭代执行：agents 按序执行，每轮输出作为
        // 下一轮输入；`exit_when` 条件为真或达到 `rounds` 上限时停止。
        // rounds 缺省为 1000（与解析器 MirrorOrchestrateKind::Loop 一致）。
        crate::mir::orchestrate::MirOrchestrateKind::Loop {
            agents,
            rounds,
            exit_when,
        } => {
            let max_rounds = rounds.unwrap_or(1000);
            let mut input_val = env.get(input_var).unwrap_or(Value::Nil);
            let mut result = Value::Nil;
            for _round in 0..max_rounds {
                for agent in agents {
                    if agent.task_body.body.is_empty() && agent.task_body.n_regs == 0 {
                        return Err(format!(
                            "orchestrate loop: agent '{}' has empty task_body (lowering missing)",
                            agent.name
                        ));
                    }
                    let mut agent_env = env.clone();
                    agent_env.define("input".to_string(), input_val.clone(), false);
                    agent_env.clock.tick(&agent.name);
                    result = crate::mir::vm::run_mir(
                        &std::sync::Arc::new(agent.task_body.clone()),
                        interp,
                        &mut agent_env,
                        effects,
                    )?;
                    for (name, val) in agent_env.iter() {
                        if env.get(&name).is_none() {
                            env.define(name, val, false);
                        }
                    }
                    input_val = result.clone();
                }
                // 退出条件：exit_when 求值为真时提前结束。
                if let Some(cond_w) = exit_when {
                    let mut cond_env = env.clone();
                    cond_env.define("input".to_string(), input_val.clone(), false);
                    cond_env.define(result_var.to_string(), result.clone(), false);
                    let cond_body =
                        crate::mir::lower::lower_block_witness_to_mir(cond_w);
                    let cond_val = crate::mir::vm::run_mir(
                        &std::sync::Arc::new(cond_body),
                        interp,
                        &mut cond_env,
                        effects,
                    )?;
                    if crate::flow::is_truthy(&cond_val) {
                        break;
                    }
                }
            }
            env.define(result_var.to_string(), result, false);
            Ok(())
        }
    }
}

// ─── v0.75.85: MoE（Mixture-of-Experts）执行 ───────────────────────
// 稀疏门控（Shazeer 2017）：router 语言面 fn 打分 → top-k 稀疏激活 →
// 专家执行 → 加权组合。顺序单轮，无超步。
// 组合规则（引擎侧 Rust，Float 自由，不受语言数值塔约束）：
//   激活专家输出全为数值 → Σ(weightᵢ × outᵢ)，weightᵢ = scoreᵢ/top-k 分和
//   （归一化 softmax 权重）。
//   含 String（模型专家）→ top-1 选择（最高分专家输出）——加权求和无意义。

/// 执行 MoE：router 打分 → top-k → 专家执行 → 加权组合 → result_var 绑定。
#[allow(clippy::too_many_arguments)] // 与 h_eval 同型（orchestrate 执行签名簇）
fn run_moe(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    experts: &[crate::mir::orchestrate::MirMoeExpert],
    router_fn: &MirFunction,
    top_k: usize,
    prompt_fn: &MirFunction,
    input_var: &str,
    result_var: &str,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    if experts.is_empty() {
        return Err("moe: experts must not be empty".to_string());
    }
    if top_k == 0 {
        return Err("moe: top_k must be >= 1".to_string());
    }

    let input_val = env.get(input_var).unwrap_or(Value::Nil);

    // 1. router 执行（语言面 fn）→ 分数 dict
    let router_val = run_mir_fn(interp, env, router_fn, effects)?;
    let scores = match interp.call_value(&router_val, vec![input_val.clone()], effects)? {
        Value::Dict(d) => d,
        other => {
            return Err(format!(
                "moe: router must return a Dict of expert scores, got {:?}",
                other
            ));
        }
    };

    // 2. top-k 稀疏：按分数降序取前 top_k 个专家
    let mut scored: Vec<(String, f64)> = Vec::new();
    for (name, score) in &scores {
        let s = value_to_f64(score).ok_or_else(|| {
            format!(
                "moe: router score for '{}' must be a number, got {:?}",
                name, score
            )
        })?;
        scored.push((name.clone(), s));
    }
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
    scored.truncate(top_k);
    if scored.is_empty() {
        return Err("moe: router returned no scores".to_string());
    }

    // 3. 激活专家执行
    let mut outputs: Vec<(String, f64, Value)> = Vec::new(); // (name, score, output)
    let mut score_sum = 0.0f64;
    for (name, score) in &scored {
        let expert = experts
            .iter()
            .find(|e| &e.name == name)
            .ok_or_else(|| format!("moe: router referenced unknown expert '{}'", name))?;
        let out = run_moe_expert(interp, env, expert, &input_val, prompt_fn, effects)?;
        score_sum += *score;
        outputs.push((name.clone(), *score, out));
    }

    // 4. 加权组合
    let result = combine_moe_outputs(&outputs, score_sum);
    env.define(result_var.to_string(), result, false);
    Ok(())
}

/// 执行单个专家：函数专家 call_value(fn, [input])；模型专家 ai.chat。
fn run_moe_expert(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    expert: &crate::mir::orchestrate::MirMoeExpert,
    input_val: &Value,
    prompt_fn: &MirFunction,
    effects: &mut crate::mir::effect::Effects,
) -> Result<Value, String> {
    let def_val = run_mir_fn(interp, env, &expert.def_fn, effects)?;
    match def_val {
        // 函数专家：Closure/Task/Compose/Partial → call_value
        Value::Closure { .. }
        | Value::Task { .. }
        | Value::Compose(_)
        | Value::Partial(_, _) => interp.call_value(&def_val, vec![input_val.clone()], effects),
        // 模型专家：{model: "..."} → ai.chat(prompt, {model})
        Value::Dict(d) => {
            let model = match d.get("model") {
                Some(Value::String(m)) => m.clone(),
                _ => {
                    return Err(format!(
                        "moe: expert '{}' dict must have a 'model' string key",
                        expert.name
                    ));
                }
            };
            // prompt 函数 → 值（含 {input} 插值，经 env 的 input 变量）
            let prompt_val = run_mir_fn(interp, env, prompt_fn, effects)?;
            let prompt_str = match prompt_val {
                Value::String(s) => s,
                other => other.to_string(),
            };
            // ai.chat 是方法调用（ai.chat(prompt, {model})），经 env 的
            // ai builtin + MethodCall 指令执行。
            let mut body: Vec<crate::mir::MirInst> = Vec::new();
            let mut nxt = 0usize;
            let ai_r = 0;
            body.push(crate::mir::MirInst::Var(ai_r, "ai".to_string()));
            nxt += 1;
            let prompt_r = nxt;
            body.push(crate::mir::MirInst::Const(
                prompt_r,
                Value::String(prompt_str),
            ));
            nxt += 1;
            let dict_r = nxt;
            let mut cfg = HashMap::new();
            cfg.insert("model".to_string(), Value::String(model));
            body.push(crate::mir::MirInst::Const(dict_r, Value::Dict(cfg)));
            nxt += 1;
            let res_r = nxt;
            body.push(crate::mir::MirInst::MethodCall(
                res_r,
                ai_r,
                "chat".to_string(),
                vec![prompt_r, dict_r],
            ));
            nxt += 1;
            let body_fn = MirFunction {
                params: vec![],
                body,
                n_regs: nxt,
                ..Default::default()
            };
            let mut expert_env = env.clone();
            crate::mir::vm::run_mir(&std::sync::Arc::new(body_fn), interp, &mut expert_env, effects)
        }
        other => Err(format!(
            "moe: expert '{}' must be a function or {{model: \"...\"}} dict, got {:?}",
            expert.name, other
        )),
    }
}

/// 组合：全数值 → 归一化加权求和；含 String → top-1 选择。
fn combine_moe_outputs(outputs: &[(String, f64, Value)], score_sum: f64) -> Value {
    let all_numeric = outputs
        .iter()
        .all(|(_, _, o)| matches!(o, Value::Int(_) | Value::Float(_)));
    if all_numeric && score_sum > 0.0 {
        let mut acc = 0.0f64;
        for (_, score, out) in outputs {
            let v = match out {
                Value::Int(i) => *i as f64,
                Value::Float(f) => *f,
                _ => 0.0,
            };
            let w = score / score_sum;
            acc += w * v;
        }
        Value::Float(acc)
    } else {
        // 含 String（模型专家）→ top-1（输出已按分数降序）
        outputs
            .first()
            .map(|(_, _, o)| o.clone())
            .unwrap_or(Value::Nil)
    }
}

/// v0.75.84: pregel 图执行公共路径（Pregel / MoA 共用）。
/// 从 h_orchestrate Pregel 分支提取：checkpoint 恢复、input 通道初始化、
/// 冲突回调、effects flush、run、result 绑定。
fn run_pregel_config(
    interp: &mut dyn MirHost,
    env: &mut Environment,
    config: crate::mir::orchestrate::MirPregelConfig,
    input_var: &str,
    result_var: &str,
    effects: &mut crate::mir::effect::Effects,
) -> Result<(), String> {
    use crate::pregel::MirPregelEngine;
    let mut engine = MirPregelEngine::new(config);

    // v0.75.84: 注入执行环境（含 builtin ai 等）— pregel agent 的 env
    // 单一来源；不注入时回落 interpreter.environment()（单测路径）。
    // `__moa_input` 携带 input_var 原始值（agent env 的 `input` 是 pregel
    // delta JSON，MoA 首层 proposer 的 `{input}` 插值需要真值）。
    // v0.95: 注入纯值快照，引擎自有所有权。
    let mut base_env = env.clone();
    if let Some(v) = env.get(input_var) {
        base_env.define("__moa_input".to_string(), v, false);
    }
    engine = engine.with_base_env(base_env);

    // v0.66: Wire PersistRuntime's checkpoint saver into the engine
    // so the auto-save block in BSP ADVANCE actually persists.
    if let Some(saver) = interp.checkpoint_saver() {
        engine = engine.with_checkpoint_saver(saver);
    }

    // v0.63: Resume from checkpoint if available
    let thread_id = "pregel"; // matches build_checkpoint default
    if let Ok(Some(cp)) = interp.load_checkpoint(thread_id) {
        engine.restore_checkpoint(&cp);
    }

    // Only init channels if starting fresh (not restored)
    if engine.current_step == 0 {
        let input_val = env.get(input_var).unwrap_or(Value::Nil);
        let mut initial = HashMap::new();
        initial.insert(input_var.to_string(), input_val);
        engine.init_channels(initial);
    }

    // v0.62: Collect conflicts via callback for exposure in result
    let captured: std::sync::Arc<parking_lot::Mutex<Vec<crate::value::Conflict>>> =
        std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
    let cb_captured = captured.clone();
    engine = engine.with_conflict_callback(std::sync::Arc::new(move |c| {
        cb_captured.lock().push(c.clone());
        true // always continue
    }));

    // v0.93: 调用方在 orchestrate 语句之前累积的 send 先注入引擎
    // （跨超步投递）。aggregator 贡献是 per-super-step 作用域 —— 引擎 run()
    // 每个超步开始都会把 acc 重置为 config 初值，故启动前的贡献无归属超步，
    // 按既有语义不投递（aggregate 只在 agent 体内执行，天然属于某个超步）。
    let pending = std::mem::take(effects);
    engine.flush_pending_sends(pending.sends);
    // 引擎内部（agent 体 + 条件求值 + combiner + master_compute）各自持有
    // 私有 Effects 并就地折叠进引擎状态（apply_effects）；orchestrate 语句
    // 本身不再向调用方传播 BSP 内部效应（send/aggregate 是引擎内部原语）。
    let result = engine.run(interp)?;

    // v0.62: Expose conflicts as a structured list
    let conflict_list: Vec<Value> = captured
        .lock()
        .iter()
        .map(|c| {
            let mut d: HashMap<String, Value> = HashMap::new();
            d.insert("key".into(), Value::String(c.key.clone()));
            d.insert("parent_value".into(), c.parent_value.clone());
            d.insert("child_value".into(), c.child_value.clone());
            Value::Dict(d)
        })
        .collect();
    env.define(
        format!("{}_conflicts", result_var),
        Value::List(conflict_list),
        false,
    );
    env.define(result_var.to_string(), result, false);
    Ok(())
}

// ─── v0.75.84: MoA（Mixture-of-Agents）pregel 图展开 ─────────────────

/// Build a Pregel config for MoA: N proposers per layer, 1 aggregator per layer.
fn build_moa_config(
    layers: usize,
    proposers: &[String],
    aggregator: &str,
    prompt_fn: &MirFunction,
    input_var: &str,
) -> Result<crate::mir::orchestrate::MirPregelConfig, String> {
    if proposers.is_empty() {
        return Err("moa: proposers list must not be empty".to_string());
    }
    if layers == 0 {
        return Err("moa: layers must be >= 1".to_string());
    }

    // v0.92: task_expr 字段类型为 MirWitness；此处不依赖具体
    // 表达式内容（实际执行走 task_body），传 Nil 占位。
    let placeholder_expr = crate::mir::witness::MirWitness {
        kind: crate::mir::witness::WitnessKind::Literal(crate::common::Literal::Nil(
            crate::common::Span::new(0, 0),
        )),
        span: crate::common::Span::new(0, 0),
    };

    let mut agents = Vec::new();
    let mut edges = Vec::new();

    // 首层 proposer 接入 @start；层间经聚合 agent 传递。
    for l in 1..=layers {
        for (i, model) in proposers.iter().enumerate() {
            let pname = format!("p_{}_{}", l, i + 1);
            let body = build_proposer_body(l, i, model, prompt_fn, input_var);
            agents.push(crate::mir::orchestrate::MirAgentDef {
                name: pname.clone(),
                task_expr: placeholder_expr.clone(),
                verify_expr: None,
                with_config: None,
                task_body: body,
                combiner_body: None,
            });
            // 边：首层从 @start，其余层从前一层聚合 agent
            let from = if l == 1 {
                "@start".to_string()
            } else {
                format!("agg_{}", l - 1)
            };
            edges.push(crate::mir::orchestrate::MirEdgeDef {
                from: from.clone(),
                to: pname.clone(),
                condition_expr: None,
                condition_body: None,
            });
            // 每 proposer → 本层聚合 agent
            edges.push(crate::mir::orchestrate::MirEdgeDef {
                from: pname,
                to: format!("agg_{}", l),
                condition_expr: None,
                condition_body: None,
            });
        }
        // 聚合 agent：读 layer_{L}_response_*（proposer Define 合并进共享 env）
        let aname = format!("agg_{}", l);
        let body = build_aggregator_body(l, aggregator, proposers.len());
        agents.push(crate::mir::orchestrate::MirAgentDef {
            name: aname.clone(),
            task_expr: placeholder_expr.clone(),
            verify_expr: None,
            with_config: None,
            task_body: body,
            combiner_body: None,
        });
        // 末层聚合 → @exit；其余层 → 下一层 proposer（上面边已建）
        if l == layers {
            edges.push(crate::mir::orchestrate::MirEdgeDef {
                from: aname,
                to: "@exit".to_string(),
                condition_expr: None,
                condition_body: None,
            });
        }
    }

    Ok(crate::mir::orchestrate::MirPregelConfig {
        agents,
        edges,
        state_schema: vec![],
        checkpoint: None,
        interrupt_points: vec![],
        adjacency: HashMap::new(),
        // v0.75.84: MoA 走共享 env 合并投递（reconcile 将 proposer Define 的
        // layer_*_response_* 合并回共享 env，聚合 agent 经 parent 链读取）—
        // 版本快照机制对首次执行不投递 delta 通道，aggregate 通道路径不可靠。
        aggregators: Vec::new(),
        master_compute: None,
    })
}

/// proposer task_body：
///   prompt → ai.chat(prompt, {model}) → Define(layer_{L}_response_{idx})
///   结果经 reconcile_outcome 合并回共享 env，聚合 agent 经 parent 链读取
///   （版本快照机制对首次执行不投递 delta 通道，共享 env 合并是可靠路径）。
fn build_proposer_body(
    layer: usize,
    proposer_idx: usize,
    model: &str,
    prompt_fn: &MirFunction,
    input_var: &str,
) -> MirFunction {
    let mut body: Vec<crate::mir::MirInst> = Vec::new();
    let mut nxt = 0usize;
    let alloc = |nxt: &mut usize| {
        let r = *nxt;
        *nxt += 1;
        r
    };

    // 所有层：先把 `input` 覆盖为 __moa_input（base_env 注入的 input_var
    // 原始值）—— pregel 把 `input` 注入为 delta JSON，用户 prompt 的
    // `{input}` 插值需要真值。
    let input_val_reg = alloc(&mut nxt);
    body.push(crate::mir::MirInst::Var(
        input_val_reg,
        "__moa_input".to_string(),
    ));
    body.push(crate::mir::MirInst::Define(
        "input".to_string(),
        input_val_reg,
    ));

    // v0.91: prompt_fn 是 parser 预 lowering 的 MirFunction，直接展开 body，
    // 不再 runtime lower_mir_exprs。
    let prompt_reg = if layer == 1 {
        body.extend(prompt_fn.body.iter().cloned());
        nxt = nxt.max(prompt_fn.n_regs);
        prompt_fn
            .body
            .last()
            .and_then(|i| i.dst())
            .unwrap_or_else(|| {
                let r = alloc(&mut nxt);
                body.push(crate::mir::MirInst::Const(r, Value::String(String::new())));
                r
            })
    } else {
        body.extend(prompt_fn.body.iter().cloned());
        nxt = nxt.max(prompt_fn.n_regs);
        let user_prompt_reg = prompt_fn
            .body
            .last()
            .and_then(|i| i.dst())
            .unwrap_or_else(|| {
                let r = alloc(&mut nxt);
                body.push(crate::mir::MirInst::Const(r, Value::String(String::new())));
                r
            });
        let sep = alloc(&mut nxt);
        body.push(crate::mir::MirInst::Const(
            sep,
            Value::String("\n\nPrevious layer response: ".to_string()),
        ));
        let prev = alloc(&mut nxt);
        body.push(crate::mir::MirInst::Var(
            prev,
            format!("agg_result_{}", layer - 1),
        ));
        let joined = alloc(&mut nxt);
        body.push(crate::mir::MirInst::BinaryOp(
            joined,
            user_prompt_reg,
            crate::common::BinaryOp::Add,
            sep,
        ));
        let joined2 = alloc(&mut nxt);
        body.push(crate::mir::MirInst::BinaryOp(
            joined2,
            joined,
            crate::common::BinaryOp::Add,
            prev,
        ));
        joined2
    };

    let ai_reg = alloc(&mut nxt);
    body.push(crate::mir::MirInst::Var(ai_reg, "ai".to_string()));

    let dict_reg = alloc(&mut nxt);
    let mut cfg = HashMap::new();
    cfg.insert("model".to_string(), Value::String(model.to_string()));
    body.push(crate::mir::MirInst::Const(dict_reg, Value::Dict(cfg)));

    let res_reg = alloc(&mut nxt);
    body.push(crate::mir::MirInst::MethodCall(
        res_reg,
        ai_reg,
        "chat".to_string(),
        vec![prompt_reg, dict_reg],
    ));

    // v0.75.84: 结果 Define 到私有 env → reconcile 合并回共享 env
    //（聚合 agent 读取的唯一可靠路径；版本快照对首次执行不投递 delta）。
    // 1-based 命名（聚合侧按 "1. " 编号展示）。
    body.push(crate::mir::MirInst::Define(
        format!("layer_{}_response_{}", layer, proposer_idx + 1),
        res_reg,
    ));

    let _ = input_var;
    MirFunction {
        params: vec![],
        body,
        n_regs: nxt.max(1),
        ..Default::default()
    }
}

/// 聚合 agent task_body：
///   读 layer_{L}_response_{1..N}（proposer Define 合并进共享 env）→ 拼接 →
///   ai.chat("Synthesize...: " + responses, {model}) → Define(agg_result_L)
///   （末层聚合结果 = engine.run 返回的 result channel = agg_layers 的 result）。
fn build_aggregator_body(layer: usize, aggregator: &str, n_proposers: usize) -> MirFunction {
    let mut body: Vec<crate::mir::MirInst> = Vec::new();
    let mut nxt = 0usize;
    let alloc = |nxt: &mut usize| {
        let r = *nxt;
        *nxt += 1;
        r
    };

    // 拼接所有 proposer 响应："1. {r1}\n2. {r2}..."
    let mut responses_reg: Option<Reg> = None;
    for i in 0..n_proposers {
        let num = alloc(&mut nxt);
        body.push(crate::mir::MirInst::Const(
            num,
            Value::String(format!("{}. ", i + 1)),
        ));
        let var = alloc(&mut nxt);
        body.push(crate::mir::MirInst::Var(
            var,
            format!("layer_{}_response_{}", layer, i + 1),
        ));
        let joined = alloc(&mut nxt);
        body.push(crate::mir::MirInst::BinaryOp(
            joined,
            num,
            crate::common::BinaryOp::Add,
            var,
        ));
        responses_reg = match responses_reg {
            None => Some(joined),
            Some(prev) => {
                let nl = alloc(&mut nxt);
                body.push(crate::mir::MirInst::Const(
                    nl,
                    Value::String("\n".to_string()),
                ));
                let sep = alloc(&mut nxt);
                body.push(crate::mir::MirInst::BinaryOp(
                    sep,
                    prev,
                    crate::common::BinaryOp::Add,
                    nl,
                ));
                let acc = alloc(&mut nxt);
                body.push(crate::mir::MirInst::BinaryOp(
                    acc,
                    sep,
                    crate::common::BinaryOp::Add,
                    joined,
                ));
                Some(acc)
            }
        };
    }
    let responses_reg = responses_reg.unwrap_or_else(|| {
        let r = alloc(&mut nxt);
        body.push(crate::mir::MirInst::Const(r, Value::String(String::new())));
        r
    });

    let c1 = alloc(&mut nxt);
    body.push(crate::mir::MirInst::Const(
        c1,
        Value::String("Synthesize these responses into a single high-quality answer: ".to_string()),
    ));
    let prompt_reg = alloc(&mut nxt);
    body.push(crate::mir::MirInst::BinaryOp(
        prompt_reg,
        c1,
        crate::common::BinaryOp::Add,
        responses_reg,
    ));

    let ai_reg = alloc(&mut nxt);
    body.push(crate::mir::MirInst::Var(ai_reg, "ai".to_string()));

    let dict_reg = alloc(&mut nxt);
    let mut cfg = HashMap::new();
    cfg.insert("model".to_string(), Value::String(aggregator.to_string()));
    body.push(crate::mir::MirInst::Const(dict_reg, Value::Dict(cfg)));

    let res_reg = alloc(&mut nxt);
    body.push(crate::mir::MirInst::MethodCall(
        res_reg,
        ai_reg,
        "chat".to_string(),
        vec![prompt_reg, dict_reg],
    ));

    // 聚合结果 Define → 共享 env（L>1 proposer 读取 agg_result_{L-1}；
    // 末层结果经 reconcile 写 result channel，engine.run 返回）。
    body.push(crate::mir::MirInst::Define(
        format!("agg_result_{}", layer),
        res_reg,
    ));

    MirFunction {
        params: vec![],
        body,
        n_regs: nxt.max(1),
        ..Default::default()
    }
}
